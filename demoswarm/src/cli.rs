use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

/// DemoSwarm lifecycle and evidence-operations manager.
#[derive(Debug, Parser)]
#[command(
    name = "demoswarm",
    version,
    about = "Install, validate, migrate, and inspect host-native DemoSwarm integrations"
)]
pub struct Cli {
    /// Project directory. Without this flag, demoswarm discovers the nearest project root.
    #[arg(long, global = true, value_name = "PATH")]
    pub project: Option<PathBuf>,

    /// Emit the stable JSON envelope and disable interactive behavior.
    #[arg(long, global = true)]
    pub json: bool,

    /// Disable color in human-readable output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Resolve and show the complete plan without durable writes or host lifecycle actions.
    #[arg(long, global = true)]
    pub dry_run: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Install one or more host-native DemoSwarm adapters.
    Install(LifecycleArgs),
    /// Update installed adapters and shared pack contracts.
    Update(LifecycleArgs),
    /// Remove selected managed adapters while preserving runs and unrelated host content.
    Uninstall(UninstallArgs),
    /// Report desired, observed, and durable run state.
    Status(PlatformFilter),
    /// Explain lifecycle drift without treating user-owned content as corruption.
    Diff(PlatformFilter),
    /// Create or validate the project-owned DemoSwarm configuration.
    Configure(ConfigureArgs),
    /// Migrate a known legacy standalone DemoSwarm installation.
    Migrate(LifecycleArgs),
    /// Validate manager, adapter, project, and run health.
    Doctor(DoctorArgs),
    /// List known hosts, detection evidence, and support maturity.
    Platforms(PlatformFilter),
    /// Inspect and deterministically maintain durable run evidence.
    Runs(RunsArgs),
    /// Report manager and local contract versions.
    Version,
}

#[derive(Debug, Clone, Args)]
pub struct LifecycleArgs {
    /// Host adapter ID. Repeat to select several hosts.
    #[arg(long = "platform", value_name = "ID")]
    pub platforms: Vec<String>,

    /// Explicitly select every detected host.
    #[arg(long)]
    pub all_detected: bool,

    /// Installation scope.
    #[arg(long, value_enum, default_value_t = Scope::Project)]
    pub scope: Scope,

    /// Exact target pack version.
    #[arg(long, value_name = "VERSION")]
    pub pack: Option<String>,

    /// Pack or host lifecycle source.
    #[arg(long, value_enum, default_value_t = SourceKind::Release)]
    pub source: SourceKind,

    /// Offline pack bundle path when `--source bundle` is selected.
    #[arg(long, value_name = "PATH")]
    pub bundle: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct UninstallArgs {
    /// Host adapter ID. Repeat to remove several hosts.
    #[arg(long = "platform", value_name = "ID")]
    pub platforms: Vec<String>,

    /// Remove every installed adapter.
    #[arg(long)]
    pub all: bool,

    /// Installation scope.
    #[arg(long, value_enum, default_value_t = Scope::Project)]
    pub scope: Scope,

    /// Remove project configuration after adapter removal. `.runs` is still preserved.
    #[arg(long)]
    pub purge_config: bool,
}

#[derive(Debug, Clone, Args, Default)]
pub struct PlatformFilter {
    /// Limit output to a host adapter ID. Repeat to select several hosts.
    #[arg(long = "platform", value_name = "ID")]
    pub platforms: Vec<String>,
}

#[derive(Debug, Clone, Args, Default)]
pub struct ConfigureArgs {
    /// Add an enabled platform entry to a newly created configuration.
    #[arg(long = "platform", value_name = "ID")]
    pub platforms: Vec<String>,
}

#[derive(Debug, Clone, Args, Default)]
pub struct DoctorArgs {
    /// Limit host checks to selected adapter IDs.
    #[arg(long = "platform", value_name = "ID")]
    pub platforms: Vec<String>,

    /// Apply only deterministic ownership-proven repairs.
    #[arg(long)]
    pub fix: bool,
}

#[derive(Debug, Args)]
pub struct RunsArgs {
    #[command(subcommand)]
    pub command: RunsCommand,
}

#[derive(Debug, Subcommand)]
pub enum RunsCommand {
    /// List run manifests and latest receipt state.
    List,
    /// Show one run and its per-flow state.
    Show { run_id: String },
    /// Validate one run or every discovered run.
    Validate { run_id: Option<String> },
    /// Rebuild the derived `.runs/index.json` cache.
    RebuildIndex,
    /// Move a complete run to the archive namespace.
    Archive { run_id: String },
    /// Create a redacted support/review export bundle.
    Export {
        run_id: String,
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Project,
    User,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Release,
    Bundle,
    Local,
    Native,
}
