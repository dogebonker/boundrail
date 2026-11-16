use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use boundrail::collect;
use boundrail::model::{Document, Granularity, Language, Source};
use clap::{Args, Parser, Subcommand};

#[path = "cli/model.rs"]
mod model_cli;
#[path = "cli/scan.rs"]
mod scan_cli;
#[path = "cli/view.rs"]
mod view_cli;

#[derive(Parser)]
#[command(
    version,
    about = "An authored architecture model checked against the code, for developers and coding agents"
)]
struct Cli {
    /// Seconds an external tool (collector, language server, agent, gh) may run before it is
    /// killed; 0 waits forever
    #[arg(long, global = true, value_name = "SECONDS", default_value_t = 600)]
    tool_timeout: u64,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create, read and describe the authored architecture model in .boundrail/model
    Model(model_cli::ModelArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    collect::request_timeout(
        (cli.tool_timeout > 0).then(|| std::time::Duration::from_secs(cli.tool_timeout)),
    );
    let Cmd::Model(args) = cli.command;
    model_cli::run(args).unwrap_or_else(|e| {
        eprintln!("error: {e:#}");
        ExitCode::from(2)
    })
}

/// Pretty JSON to `output`, or stdout.
fn write(output: Option<&Path>, value: &impl serde::Serialize) -> Result<()> {
    let json = serde_json::to_string_pretty(&serde_json::to_value(value)?)? + "\n";
    match output {
        Some(path) => write_file(path, json.as_bytes()),
        None => {
            print!("{json}");
            Ok(())
        }
    }
}

/// Replaces `path` atomically, so an interrupted run never leaves a truncated output.
fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    boundrail::files::write_atomic(path, bytes)
        .with_context(|| format!("writing {}", path.display()))
}
