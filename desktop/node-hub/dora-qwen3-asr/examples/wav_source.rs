//! E2E test source: feeds a 16kHz WAV file into the dataflow as one committed
//! speech segment (transcription_mode=final), followed by question_ended.

use eyre::{self, Context as _};
use dora_node_api::{DoraNode, IntoArrow, Parameter};
use std::collections::BTreeMap;

fn main() -> eyre::Result<()> {
    let wav_path = std::env::var("WAV_PATH").expect("WAV_PATH env required");
    // Spoken language of the WAV; flows into transcription metadata so the
    // translator picks the right direction. Defaults to zh (baseline test).
    let language = std::env::var("SOURCE_LANGUAGE").unwrap_or_else(|_| "zh".to_string());

    let mut reader = hound::WavReader::open(&wav_path).wrap_err("failed to open wav")?;
    let spec = reader.spec();
    if spec.sample_rate != 16000 || spec.channels != 1 {
        return Err(eyre::eyre!(
            "expected 16kHz mono wav, got {}Hz {}ch",
            spec.sample_rate,
            spec.channels
        ));
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect(),
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
    };
    println!(
        "wav_source: {} samples ({:.1}s)",
        samples.len(),
        samples.len() as f32 / 16000.0
    );

    let (mut node, _events) = DoraNode::init_from_env()?;

    let mut meta = BTreeMap::new();
    meta.insert("sample_rate".to_string(), Parameter::Integer(16000));
    meta.insert("language".to_string(), Parameter::String(language));
    meta.insert("question_id".to_string(), Parameter::Integer(1));
    meta.insert("burst_id".to_string(), Parameter::Integer(1));
    meta.insert(
        "transcription_mode".to_string(),
        Parameter::String("final".to_string()),
    );
    meta.insert(
        "segment_reason".to_string(),
        Parameter::String("speech_end".to_string()),
    );
    node.send_output("audio".into(), meta, samples.into_arrow())?;
    println!("wav_source: audio sent");

    let mut end_meta = BTreeMap::new();
    end_meta.insert("question_id".to_string(), Parameter::Integer(1));
    let empty: Vec<String> = vec![];
    node.send_output("question_ended".into(), end_meta, empty.into_arrow())?;
    println!("wav_source: question_ended sent");

    // Give downstream nodes a moment to consume before we exit.
    std::thread::sleep(std::time::Duration::from_secs(120));
    Ok(())
}
