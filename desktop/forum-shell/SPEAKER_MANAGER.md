# 可选匿名说话人宿主（F09）

`SpeakerManager::new(core, data_root, resources, budget)` 只创建默认关闭的后台协调线程，不打开音频设备、不下载模型，也不访问旧 Forum server。与分析模型共享ResourceBudget，一次只占用一个后台槽位；实时字幕准备或积压会取消ECAPA并保留候选段供后续重试。

`client.enable(session_id, model_path: Option<PathBuf>)` 明确选择本次处理的场次和可选本地模型目录；不会更改环境变量或其他场次。没有显式目录时使用FORUM_SPEAKER_MODEL_PATH或data_root/models/ecapa，不隐式查找开发仓库的data/ecapa_model。包内入口为resources/speaker-worker/python/bin/python3.12，开发时可以显式指定FORUM_SPEAKER_PYTHON和FORUM_SPEAKER_WORKER_SOURCE。

`status()` 返回enabled、session_id、state、notice、active_segment、completed、skipped。list(session_id)返回独立SpeakerAssignment；correct(command)只接受Human origin，须注明操作者和原因。标签只能是unknown、overlap或session内anonymous UUID，不能据此推断真人身份。disable()、切换场次及shutdown会取消正在运行的owned进程组；shutdown_complete同时检查协调线程、进程退出和core待提交计数。App开始新场次前应先disable旧场次。

扫描按core固定cursor分页，每次100段，仅选择已成功落盘的ASR原文。读取单轨sessions/<id>/audio，双轨读取audio/tracks/<trackid>。核对私有目录、RecordingManifest身份、音轨、segment UUID、音频时间范围、SHA、长度及有限f32值，再转成16kHz mono s16le，写入独立speaker-jobs/<job>/<attempt>/segment.pcm。无PCM、录音关闭或校验失败时保留未知与明确状态，绝不以轨道名假充声纹结果。

每次attempt启动独立worker，30秒预算涵盖输入校验/写入、初始化、模型载入和推理；每100ms检查取消、共享资源压力和deadline。校验响应的job/session/track/segment/source revision、模型/PCM摘要、质量字段、192维归一化向量。Rust确认实际owned进程退出后才允许core提交，且core闭包内再次检查启用代次，使core队列延迟/ACK超时之后也不能绕过disable。提交由core再检查原文版本和人工锁。临时PCM在所有返回路径清理，不复制永久录音。

没有可信自动重叠检测器时host不发送overlap=true，也不把双轨时间重叠直接当成多人同麦。人工overlap标签受core保护。ECAPA门槛和聚类阈值仍需真人声学评测。

验证入口：

```sh
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --manifest-path desktop/Cargo.toml --locked --offline -p forum-shell --example speaker_host_probe
```

默认9项测试使用合成PCM、临时DB和测试Python协议进程，覆盖私有录音映射/拒绝路径跳转、默认关闭、结果范围/质量、实际进程退出后才提交、disable/资源抢占/host deadline和排队迟到提交。真实ECAPA测试默认ignored，显式设置FORUM_SPEAKER_PYTHON、FORUM_SPEAKER_PROBE_MODEL、FORUM_SPEAKER_PROBE_REPORT后，用同一命令追加`installed_ecapa_manager_scans_recording_and_commits_real_embedding -- --ignored`。

2026-09-17的真实host→installed runtime→ECAPA→core合成音频探针用/tmp/forum-speaker-runtime-2及显式已安装模型，4.106秒完成1段，模型缓存大小/mtime不变，关闭检查通过。证据在artifacts/local/f09-speaker-host/report.json。这证明生产宿主链路可执行，不证明真人区分准确率、重叠检测或C3正式验收。
