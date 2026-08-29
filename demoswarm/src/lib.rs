#![forbid(unsafe_code)]
#![doc = "Shared library for the DemoSwarm lifecycle and evidence-operations manager."]

pub mod cli;
mod commands;
pub mod model;
pub mod project;

use clap::Parser;
use model::{CommandResult, Diagnostic, EXIT_ENVIRONMENT, EXIT_USAGE, Envelope, Severity};
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};

/// Execute the manager with an arbitrary argument iterator and return a stable process exit code.
pub fn run<I, T>(args: I) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let raw: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let json_requested = raw.iter().any(|argument| argument == OsStr::new("--json"));

    let cli = match cli::Cli::try_parse_from(raw) {
        Ok(cli) => cli,
        Err(error) => {
            if json_requested {
                let result = CommandResult::failure(
                    "parse",
                    false,
                    None,
                    Value::Null,
                    vec!["Command-line arguments are invalid.".to_string()],
                    vec![Diagnostic::new(
                        "DSW-USAGE-001",
                        Severity::Error,
                        "command-line",
                        error.to_string(),
                    )],
                    EXIT_USAGE,
                );
                let _ = write_json(&result.envelope);
                return result.exit_code;
            }
            let _ = error.print();
            return EXIT_USAGE;
        }
    };

    let project = match project::ProjectContext::discover(cli.project.as_deref()) {
        Ok(project) => project,
        Err(error) => {
            let result = CommandResult::failure(
                command_name(&cli.command),
                cli.dry_run,
                cli.project
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                Value::Null,
                vec![error.to_string()],
                vec![Diagnostic::new(
                    "DSW-PROJECT-004",
                    Severity::Error,
                    "project-root",
                    error.to_string(),
                )],
                EXIT_ENVIRONMENT,
            );
            let _ = render(&result, cli.json);
            return result.exit_code;
        }
    };

    let result = commands::execute(&cli.command, &project, cli.dry_run);
    if render(&result, cli.json).is_err() {
        return EXIT_ENVIRONMENT;
    }
    result.exit_code
}

fn command_name(command: &cli::Commands) -> &'static str {
    match command {
        cli::Commands::Install(_) => "install",
        cli::Commands::Update(_) => "update",
        cli::Commands::Uninstall(_) => "uninstall",
        cli::Commands::Status(_) => "status",
        cli::Commands::Diff(_) => "diff",
        cli::Commands::Configure(_) => "configure",
        cli::Commands::Migrate(_) => "migrate",
        cli::Commands::Doctor(_) => "doctor",
        cli::Commands::Platforms(_) => "platforms",
        cli::Commands::Runs(_) => "runs",
        cli::Commands::Version => "version",
    }
}

fn render(result: &CommandResult, json: bool) -> io::Result<()> {
    if json {
        return write_json(&result.envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for line in &result.human_lines {
        writeln!(writer, "{line}")?;
    }
    for diagnostic in &result.envelope.diagnostics {
        writeln!(
            writer,
            "[{:?} {}] {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        )?;
    }
    Ok(())
}

fn write_json(envelope: &Envelope) -> io::Result<()> {
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer_pretty(&mut writer, envelope).map_err(io::Error::other)?;
    writeln!(writer)
}
