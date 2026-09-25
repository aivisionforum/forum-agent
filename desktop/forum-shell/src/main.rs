//! AI Vision Forum - Standalone Live Translation Application
//!
//! A standalone desktop application for live speech translation.

mod app;
mod audio_inputs;
mod analysis;
mod lan_manager;
mod speaker_manager;
mod dataflow;
mod identity;
mod models;
mod meeting;
mod power_activity;
mod preferences;
mod runtime;
mod usage;

use clap::Parser;

#[derive(Parser, Debug, Default, Clone)]
#[command(name = "forum-shell")]
#[command(about = "AI Vision Forum")]
#[command(version = env!("CARGO_PKG_VERSION"))]
pub struct Args {
    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    pub log_level: String,

    /// Dora dataflow YAML file path
    #[arg(short, long)]
    pub dataflow: Option<String>,
}

impl Args {
    /// Get the log filter string for env_logger
    pub fn log_filter(&self) -> &str {
        &self.log_level
    }
}

fn main() {
    // Parse command-line arguments
    let args = Args::parse();

    // Configure logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(args.log_filter()))
        .init();

    log::info!("Starting AI Vision Forum v{}", env!("CARGO_PKG_VERSION"));
    log::debug!("CLI args: {:?}", args);

    if let Some(ref dataflow) = args.dataflow {
        log::info!("Using dataflow: {}", dataflow);
    }

    app::run(args);
}
