// Included in aec_input.rs to share the owned CPAL device wrapper.
fn process_dual_packets(
    group: &mut crate::dual_capture::DualCapture,
    queues: &mut [std::collections::VecDeque<crate::audio_clock::TimedPcm>],
    clocks: &mut Option<Vec<crate::audio_clock::ClockMapper>>,
    echo: &mut crate::audio_clock::EchoReference,
    final_flush: bool,
    shared: &SharedDoraState,
) -> anyhow::Result<()> {
    if clocks.is_none() {
        if queues.iter().any(|q| q.is_empty()) {
            return Ok(());
        }
        let epoch = queues.iter().map(|q| q.front().unwrap().at).max().unwrap();
        *clocks = Some(vec![
            crate::audio_clock::ClockMapper::new(epoch),
            crate::audio_clock::ClockMapper::new(epoch),
        ]);
    }
    for index in [1, 0] {
        while queues[index].len() > 1 || final_flush && !queues[index].is_empty() {
            let packet = queues[index].pop_front().unwrap();
            let next = queues[index].front().map(|p| p.at);
            let aligned = clocks.as_mut().unwrap()[index].map(packet, next)?;
            // Persist gain before reference cancellation; replay never reapplies it.
            let samples = shared.process_input(&aligned.samples);
            if index == 1 {
                echo.push(&samples);
                group.process(
                    index,
                    &samples,
                    &samples,
                    aligned.gap_before,
                )?;
            } else {
                let processed = echo.filter(&samples);
                group.process(index, &samples, &processed, aligned.gap_before)?;
            }
        }
    }
    Ok(())
}

impl AecInputBridge {
    fn run_reliable_dual(
        node_id: String,
        state: Arc<RwLock<BridgeState>>,
        shared_state: Option<Arc<SharedDoraState>>,
        stop_receiver: Receiver<()>,
        is_recording: Arc<AtomicBool>,
        context: crate::CaptureContext,
    ) {
        let Some(shared) = shared_state else {
            *state.write() = BridgeState::Error;
            return;
        };
        #[cfg(not(target_os = "macos"))]
        {
            shared.capture_progress.set(crate::CaptureProgress {
                devices_released: true,
                error: Some("双轨采集需要 macOS 系统音频支持".into()),
                ..Default::default()
            });
            return;
        }
        #[cfg(target_os = "macos")]
        {
            let mut progress = crate::CaptureProgress::default();
            shared.capture_progress.set(progress.clone());
            let mut connection = None;
            let mut group: Option<crate::dual_capture::DualCapture> = None;
            let mut mic: Option<CpalMicCapture> = None;
            let mut system: Option<crate::widgets::screencapture_input::ScreenCaptureInput> = None;
            let mut teardown = false;
            let mut replay_done = false;
            let mut queues = vec![
                std::collections::VecDeque::new(),
                std::collections::VecDeque::new(),
            ];
            let mut clocks = None;
            let mut echo = crate::audio_clock::EchoReference::default();
            let dispatch = |node: &mut DoraNode,
                            meta: &forum_runtime::audio::SegmentMeta,
                            pcm: &[f32]|
             -> anyhow::Result<()> {
                let mut params = BTreeMap::new();
                params.insert("sample_rate".into(), Parameter::Integer(16000));
                params.insert(
                    "forum_segment".into(),
                    Parameter::String(serde_json::to_string(meta)?),
                );
                node.send_output("audio_segment".into(), params, pcm.to_vec().into_arrow())
                    .map_err(|e| anyhow::anyhow!("ASR dispatch failed: {e}"))
            };
            let mut result = (|| -> anyhow::Result<()> {
                connection = Some(crate::dynamic_node_endpoint::init_from_shared(
                    Some(&shared),
                    NodeId::from(node_id.clone()),
                )?);
                shared.add_bridge(node_id.clone());
                *state.write() = BridgeState::Connected;
                group = Some(crate::dual_capture::DualCapture::open(context.clone())?);
                let group = group.as_mut().unwrap();
                let (node, events) = connection.as_mut().unwrap();
                let deadline = Instant::now() + Duration::from_secs(120);
                while !group.ready()? {
                    if stop_receiver.try_recv().is_ok() {
                        teardown = true;
                        return Ok(());
                    }
                    if shared.capture_stop_requested.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    anyhow::ensure!(Instant::now() < deadline, "ASR readiness barrier timed out");
                    thread::sleep(Duration::from_millis(50));
                }
                if context.replay_only {
                    progress.devices_released = true;
                    shared.capture_progress.set(progress.clone());
                    let client =
                        forum_runtime::RuntimeClient::new(context.runtime.endpoint.clone());
                    group.replay(|meta, pcm| {
                        anyhow::ensure!(
                            !shared.capture_stop_requested.load(Ordering::Acquire),
                            "dual recovery cancelled"
                        );
                        dispatch(node, meta, pcm)?;
                        crate::reliable_capture::wait_for_final(&client, meta, || {
                            if stop_receiver.try_recv().is_ok() {
                                teardown = true;
                            }
                            teardown || shared.capture_stop_requested.load(Ordering::Acquire)
                        })
                    })?;
                    replay_done = true;
                    return Ok(());
                }
                if shared.capture_stop_requested.load(Ordering::Acquire) {
                    return Ok(());
                }
                system = Some(
                    crate::widgets::screencapture_input::ScreenCaptureInput::new()
                        .map_err(anyhow::Error::msg)?,
                );
                system
                    .as_mut()
                    .unwrap()
                    .start()
                    .map_err(anyhow::Error::msg)?;
                if shared.capture_stop_requested.load(Ordering::Acquire) {
                    return Ok(());
                }
                mic = Some(CpalMicCapture::with_device(None).map_err(anyhow::Error::msg)?);
                mic.as_mut().unwrap().start().map_err(anyhow::Error::msg)?;
                // Begin one meeting clock after both hardware starts. Do not wait
                // for remote speech and discard an earlier local speaker.
                let epoch = Instant::now();
                clocks = Some(vec![
                    crate::audio_clock::ClockMapper::new(epoch),
                    crate::audio_clock::ClockMapper::new(epoch),
                ]);
                progress.started = true;
                is_recording.store(true, Ordering::Release);
                shared.mic.set_recording(true);
                shared.capture_progress.set(progress.clone());
                loop {
                    if stop_receiver.try_recv().is_ok() {
                        teardown = true;
                        break;
                    }
                    if shared.capture_stop_requested.load(Ordering::Acquire) {
                        break;
                    }
                    if let Some(error) = mic.as_ref().unwrap().capture_error.lock().take() {
                        anyhow::bail!("microphone device failed: {error}");
                    }
                    if let Some(error) = system.as_ref().unwrap().take_clock_error() {
                        anyhow::bail!(error);
                    }
                    queues[0].extend(mic.as_ref().unwrap().get_timed_audio());
                    queues[1].extend(system.as_ref().unwrap().get_timed_audio());
                    process_dual_packets(group, &mut queues, &mut clocks, &mut echo, false, &shared)?;
                    group.pump(|meta, pcm| dispatch(node, meta, pcm))?;
                    shared.capture_progress.set(crate::CaptureProgress {
                        started: true,
                        ..group.progress()
                    });
                    if let Ok(Event::Stop(_)) = events.try_recv() {
                        teardown = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(())
            })();
            let stopped_at = Instant::now();
            if let Some(clocks) = clocks.as_mut() {
                for clock in clocks {
                    clock.stop_at(stopped_at);
                }
            }
            // Release both hardware streams before journaling their final buffers.
            if let Some(mut source) = mic.take() {
                drop(source.stream.take());
                source.is_recording = false;
                queues[0].extend(source.get_timed_audio());
            }
            if let Some(mut source) = system.take() {
                let release_started = Instant::now();
                loop {
                    match source.stop_and_drain() {
                        Ok(_) => {
                            queues[1].extend(source.get_timed_audio());
                            break;
                        }
                        Err(error) => {
                            if stop_receiver.try_recv().is_ok() {
                                teardown = true;
                            }
                            let overdue = release_started.elapsed() > Duration::from_secs(5);
                            progress.error = Some(if overdue {
                                format!("系统音频停止超过 5 秒，仍持有设备并继续回收：{error}")
                            } else {
                                error
                            });
                            shared.capture_progress.set(progress.clone());
                            // A failed framework release is not permission to drop
                            // ownership. The host surfaces failure and blocks reuse.
                            thread::sleep(Duration::from_millis(if overdue { 1000 } else { 250 }));
                        }
                    }
                }
            }
            is_recording.store(false, Ordering::Release);
            shared.mic.set_recording(false);
            progress.devices_released = true;
            if result.is_ok() && !replay_done && !context.replay_only {
                if let (Some(group), Some((node, _))) = (group.as_mut(), connection.as_mut()) {
                    result = (|| -> anyhow::Result<()> {
                        process_dual_packets(group, &mut queues, &mut clocks, &mut echo, true, &shared)?;
                        // Seal the last observed PCM within the stop boundary,
                        // not a fictitious wall-clock tail that neither source saw.
                        let final_sample = group.progress().final_sample;
                        group.seal_at(final_sample)?;
                        progress.capture_sealed = true;
                        // Persist drained tails before publishing release: forced
                        // host cleanup must not discard the final in-memory PCM.
                        shared.capture_progress.set(progress.clone());
                        while !group.empty() && !teardown {
                            if stop_receiver.try_recv().is_ok() {
                                teardown = true;
                                break;
                            }
                            group.pump(|meta, pcm| dispatch(node, meta, pcm))?;
                            thread::sleep(Duration::from_millis(50));
                        }
                        Ok(())
                    })();
                }
            }
            if let Some(group) = &group {
                let p = group.progress();
                progress.final_sample = p.final_sample;
                progress.segments_closed = p.segments_closed;
                progress.outbox_pending = p.outbox_pending;
            }
            progress.capture_sealed |= replay_done;
            if let Err(error) = result {
                progress.error = Some(format!("{error:#}"));
                shared.set_error(progress.error.clone());
            }
            shared.capture_progress.set(progress);
            if let Some((_, events)) = connection.as_mut() {
                while !teardown {
                    if stop_receiver.try_recv().is_ok() {
                        break;
                    }
                    match events.try_recv() {
                        Ok(Event::Stop(_)) | Err(dora_node_api::TryRecvError::Closed) => break,
                        _ => {}
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
            drop(connection);
            shared.remove_bridge(&node_id);
            *state.write() = BridgeState::Disconnected;
        }
    }
}
