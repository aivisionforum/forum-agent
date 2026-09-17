//! E2E test sink: prints transcription/source/translation events and exits
//! after the first committed non-empty translation (or after the deadline).

use arrow::array::{Array, StringArray};
use eyre;
use dora_node_api::{DoraNode, Event};
use std::time::{Duration, Instant};

fn main() -> eyre::Result<()> {
    let (_node, mut events) = DoraNode::init_from_env()?;
    let deadline = Instant::now() + Duration::from_secs(240);

    while Instant::now() < deadline {
        let Some(event) = events.recv() else { break };
        let Event::Input { id, data, metadata } = event else {
            continue;
        };

        let text = data
            .as_any()
            .downcast_ref::<StringArray>()
            .and_then(|a| {
                if a.len() == 0 {
                    None
                } else {
                    Some(a.value(0).to_string())
                }
            })
            .unwrap_or_default();
        let status = metadata
            .parameters
            .get("session_status")
            .and_then(|p| match p {
                dora_node_api::Parameter::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();

        println!("[sink] {id} (status={status}): {text}");

        if id.as_str() == "translation" && !text.trim().is_empty() && status != "streaming" {
            println!("[sink] committed translation received, exiting");
            return Ok(());
        }
    }
    Ok(())
}
