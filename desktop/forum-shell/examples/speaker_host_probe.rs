//! Host lifecycle/PCM tests only; no capture device or default model work.
#![allow(dead_code)]
#[path = "../src/speaker_manager.rs"]
mod speaker_manager;
fn main() {
    println!("Run cargo test -p forum-shell --example speaker_host_probe for isolated host checks.");
}
