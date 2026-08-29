use crate::cli::{Commands, ConfigureArgs, DoctorArgs, PlatformFilter, RunsCommand};
use crate::model::{
    CommandResult, Diagnostic, EXIT_ENVIRONMENT, EXIT_INVALID_STATE, EXIT_UNSUPPORTED, Severity,
};
use crate::project::ProjectContext;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const FLOW_ORDER: [&str; 7] = [
    "signal", "plan", "build", "review", "gate", "deploy", "wisdom",
];

#[derive(Debug, Clone, Serialize)]
struct PlatformStatus {
    id: &'static str,
    display_name: &'static str,
    support: &'static str,
    executable: &'static str,
    detected: bool,
    executable_path: Option<String>,
    project_markers: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProjectConfig {
    schema_version: u32,
    repository: RepositoryConfig,
    #[serde(default)]
    platforms: BTreeMap<String, PlatformConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RepositoryConfig {
    provider: String,
    default_branch: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PlatformConfig {
    enabled: bool,
    scope: String,
}

#[derive(Debug, Deserialize)]
struct RunManifest {
    #[allow(dead_code)]
    schema_version: Value,
    run_id: String,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    producer: Option<Producer>,
}

#[derive(Debug, Deserialize)]
struct Producer {
    host: String,
    #[serde(default)]
    adapter_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Receipt {
    #[allow(dead_code)]
    #[serde(default)]
    schema_version: Option<Value>,
    run_id: String,
    flow: String,
    #[serde(default)]
    completion: Option<String>,
    #[serde(default)]
    verification: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    generated_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct RunSummary {
    run_id: String,
    host: Option<String>,
    adapter_version: Option<String>,
    last_flow: Option<String>,
    completion: Option<String>,
    verification: Option<String>,
    updated_at: Option<String>,
    legacy: bool,
}

pub fn execute(command: &Commands, project: &ProjectContext, dry_run: bool) -> CommandResult {
    match command {
        Commands::Version => version(project, dry_run),
        Commands::Platforms(filter) => platforms(project, filter, dry_run),
        Commands::Status(filter) => status(project, filter, dry_run),
        Commands::Doctor(args) => doctor(project, args, dry_run),
        Commands::Configure(args) => configure(project, args, dry_run),
        Commands::Runs(args) => runs(project, &args.command, dry_run),
        Commands::Diff(_) => unsupported(
            "diff",
            project,
            dry_run,
            "Managed-file diff requires the ownership ledger from transaction-engine issue #3.",
        ),
        Commands::Install(_) => unsupported(
            "install",
            project,
            dry_run,
            "Installation requires authenticated pack resolution and the shared transaction engine.",
        ),
        Commands::Update(_) => unsupported(
            "update",
            project,
            dry_run,
            "Update requires authenticated pack resolution, ownership state, and rollback support.",
        ),
        Commands::Uninstall(_) => unsupported(
            "uninstall",
            project,
            dry_run,
            "Uninstall requires ownership-proven state and shared-asset reference handling.",
        ),
        Commands::Migrate(_) => unsupported(
            "migrate",
            project,
            dry_run,
            "Legacy migration requires the historical fingerprint catalog and transaction journal.",
        ),
    }
}

fn version(project: &ProjectContext, dry_run: bool) -> CommandResult {
    let version = env!("CARGO_PKG_VERSION");
    CommandResult::success(
        "version",
        dry_run,
        Some(project.display_root()),
        json!({
            "manager": {
                "package": "demoswarm",
                "version": version
            },
            "pack": null,
            "schemas": {
                "config": 1,
                "run": 1,
                "receipt": 2,
                "output": 1
            }
        }),
        vec![
            format!("demoswarm {version}"),
            "Pack: not resolved for this command".to_string(),
            "Schemas: config 1, run 1, receipt 2, output 1".to_string(),
        ],
        Vec::new(),
    )
}

fn platforms(
    project: &ProjectContext,
    filter: &PlatformFilter,
    dry_run: bool,
) -> CommandResult {
    let requested: BTreeSet<&str> = filter.platforms.iter().map(String::as_str).collect();
    let all = detect_platforms(project.root());
    let selected: Vec<PlatformStatus> = all
        .into_iter()
        .filter(|platform| requested.is_empty() || requested.contains(platform.id))
        .collect();

    let unknown: Vec<&str> = requested
        .iter()
        .copied()
        .filter(|id| !selected.iter().any(|platform| platform.id == *id))
        .collect();

    if !unknown.is_empty() {
        let diagnostic = Diagnostic::new(
            "DSW-PLATFORM-001",
            Severity::Error,
            "platform-selection",
            format!("unknown platform IDs: {}", unknown.join(", ")),
        )
        .with_remediation(
            "Run `demoswarm platforms` to list first-party platform IDs.",
            false,
        );
        return CommandResult::failure(
            "platforms",
            dry_run,
            Some(project.display_root()),
            json!({ "platforms": selected }),
            vec![diagnostic.message.clone()],
            vec![diagnostic],
            EXIT_UNSUPPORTED,
        );
    }

    let mut lines = vec!["Known DemoSwarm hosts:".to_string()];
    for platform in &selected {
        let detected = if platform.detected {
            "detected"
        } else {
            "not detected"
        };
        lines.push(format!(
            "  {:<12} {:<12} {}",
            platform.id, platform.support, detected
        ));
    }

    CommandResult::success(
        "platforms",
        dry_run,
        Some(project.display_root()),
        json!({ "platforms": selected }),
        lines,
        Vec::new(),
    )
}

fn status(project: &ProjectContext, filter: &PlatformFilter, dry_run: bool) -> CommandResult {
    let config_path = project.root().join(".demoswarm/config.toml");
    let state_path = project.root().join(".demoswarm/state.toml");
    let runs_path = project.root().join(".runs");

    let (config_status, config_error) = inspect_toml(&config_path);
    let (state_status, state_error) = inspect_toml(&state_path);
    let run_summaries = scan_runs(project.root()).unwrap_or_default();
    let legacy_paths = legacy_runtime_paths(project.root());

    let requested: BTreeSet<&str> = filter.platforms.iter().map(String::as_str).collect();
    let platforms: Vec<PlatformStatus> = detect_platforms(project.root())
        .into_iter()
        .filter(|platform| requested.is_empty() || requested.contains(platform.id))
        .collect();

    let mut diagnostics = Vec::new();
    if let Some(message) = config_error {
        diagnostics.push(Diagnostic::new(
            "DSW-CONFIG-002",
            Severity::Error,
            ".demoswarm/config.toml",
            message,
        ));
    }
    if let Some(message) = state_error {
        diagnostics.push(Diagnostic::new(
            "DSW-STATE-002",
            Severity::Error,
            ".demoswarm/state.toml",
            message,
        ));
    }
    if !legacy_paths.is_empty() {
        diagnostics.push(
            Diagnostic::new(
                "DSW-LEGACY-001",
                Severity::Warning,
                "legacy-runtime",
                format!(
                    "legacy DemoSwarm runtime paths are still present: {}",
                    legacy_paths.join(", ")
                ),
            )
            .with_remediation("Run `demoswarm migrate --dry-run` once migration support lands.", false),
        );
    }

    let ok = !diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error);
    let lines = vec![
        format!("Project: {}", project.display_root()),
        format!("Discovery: {}", project.discovery()),
        format!("Config: {config_status}"),
        format!("Manager state: {state_status}"),
        format!(
            "Runs: {}{}",
            run_summaries.len(),
            if runs_path.is_dir() { "" } else { " (directory absent)" }
        ),
        format!("Legacy runtime paths: {}", legacy_paths.len()),
    ];

    let data = json!({
        "discovery": project.discovery().to_string(),
        "config": { "path": display_relative(project.root(), &config_path), "status": config_status },
        "state": { "path": display_relative(project.root(), &state_path), "status": state_status },
        "platforms": platforms,
        "runs": { "count": run_summaries.len(), "items": run_summaries },
        "legacy_runtime_paths": legacy_paths,
    });

    if ok {
        CommandResult::success(
            "status",
            dry_run,
            Some(project.display_root()),
            data,
            lines,
            diagnostics,
        )
    } else {
        CommandResult::failure(
            "status",
            dry_run,
            Some(project.display_root()),
            data,
            lines,
            diagnostics,
            EXIT_INVALID_STATE,
        )
    }
}

fn configure(project: &ProjectContext, args: &ConfigureArgs, dry_run: bool) -> CommandResult {
    let known: BTreeSet<&str> = platform_definitions()
        .iter()
        .map(|definition| definition.id)
        .collect();
    let unknown: Vec<&str> = args
        .platforms
        .iter()
        .map(String::as_str)
        .filter(|id| !known.contains(id))
        .collect();
    if !unknown.is_empty() {
        let diagnostic = Diagnostic::new(
            "DSW-CONFIG-001",
            Severity::Error,
            "platform-selection",
            format!("unknown platform IDs: {}", unknown.join(", ")),
        );
        return CommandResult::failure(
            "configure",
            dry_run,
            Some(project.display_root()),
            json!({ "created": false }),
            vec![diagnostic.message.clone()],
            vec![diagnostic],
            EXIT_UNSUPPORTED,
        );
    }

    let path = project.root().join(".demoswarm/config.toml");
    if path.exists() {
        let (status, error) = inspect_toml(&path);
        let diagnostics = error
            .map(|message| {
                vec![Diagnostic::new(
                    "DSW-CONFIG-002",
                    Severity::Error,
                    ".demoswarm/config.toml",
                    message,
                )]
            })
            .unwrap_or_default();
        if diagnostics.is_empty() {
            return CommandResult::success(
                "configure",
                dry_run,
                Some(project.display_root()),
                json!({ "created": false, "status": status, "path": ".demoswarm/config.toml" }),
                vec!["Configuration already exists and parses successfully.".to_string()],
                diagnostics,
            );
        }
        return CommandResult::failure(
            "configure",
            dry_run,
            Some(project.display_root()),
            json!({ "created": false, "status": status, "path": ".demoswarm/config.toml" }),
            vec!["Existing configuration is invalid; it was not replaced.".to_string()],
            diagnostics,
            EXIT_INVALID_STATE,
        );
    }

    let mut platform_config = BTreeMap::new();
    for id in &args.platforms {
        platform_config.insert(
            id.clone(),
            PlatformConfig {
                enabled: true,
                scope: "project".to_string(),
            },
        );
    }
    let config = ProjectConfig {
        schema_version: 1,
        repository: RepositoryConfig {
            provider: detect_repository_provider(project.root()),
            default_branch: "main".to_string(),
        },
        platforms: platform_config,
    };
    let content = match toml::to_string_pretty(&config) {
        Ok(content) => content,
        Err(error) => {
            let diagnostic = Diagnostic::new(
                "DSW-CONFIG-003",
                Severity::Error,
                ".demoswarm/config.toml",
                format!("could not render configuration: {error}"),
            );
            return CommandResult::failure(
                "configure",
                dry_run,
                Some(project.display_root()),
                json!({ "created": false }),
                vec![diagnostic.message.clone()],
                vec![diagnostic],
                EXIT_ENVIRONMENT,
            );
        }
    };

    if dry_run {
        return CommandResult::success(
            "configure",
            true,
            Some(project.display_root()),
            json!({
                "created": false,
                "would_create": ".demoswarm/config.toml",
                "content": content,
            }),
            vec!["Would create .demoswarm/config.toml; no files were written.".to_string()],
            Vec::new(),
        );
    }

    let parent = match path.parent() {
        Some(parent) => parent,
        None => {
            let diagnostic = Diagnostic::new(
                "DSW-CONFIG-004",
                Severity::Error,
                ".demoswarm/config.toml",
                "configuration path has no parent directory",
            );
            return CommandResult::failure(
                "configure",
                false,
                Some(project.display_root()),
                json!({ "created": false }),
                vec![diagnostic.message.clone()],
                vec![diagnostic],
                EXIT_ENVIRONMENT,
            );
        }
    };
    if let Err(error) = fs::create_dir_all(parent) {
        let diagnostic = Diagnostic::new(
            "DSW-CONFIG-005",
            Severity::Error,
            ".demoswarm",
            format!("could not create configuration directory: {error}"),
        );
        return CommandResult::failure(
            "configure",
            false,
            Some(project.display_root()),
            json!({ "created": false }),
            vec![diagnostic.message.clone()],
            vec![diagnostic],
            EXIT_ENVIRONMENT,
        );
    }

    let write_result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| {
            file.write_all(content.as_bytes())?;
            file.sync_all()
        });
    if let Err(error) = write_result {
        let diagnostic = Diagnostic::new(
            "DSW-CONFIG-006",
            Severity::Error,
            ".demoswarm/config.toml",
            format!("could not create configuration without overwriting existing content: {error}"),
        );
        return CommandResult::failure(
            "configure",
            false,
            Some(project.display_root()),
            json!({ "created": false }),
            vec![diagnostic.message.clone()],
            vec![diagnostic],
            EXIT_ENVIRONMENT,
        );
    }

    CommandResult::success(
        "configure",
        false,
        Some(project.display_root()),
        json!({ "created": true, "path": ".demoswarm/config.toml" }),
        vec!["Created .demoswarm/config.toml.".to_string()],
        Vec::new(),
    )
}

fn doctor(project: &ProjectContext, args: &DoctorArgs, dry_run: bool) -> CommandResult {
    let mut diagnostics = Vec::new();
    diagnostics.push(Diagnostic::new(
        "DSW-PROJECT-001",
        Severity::Pass,
        "project-root",
        format!("project root resolved by {}", project.discovery()),
    ));

    if project.root().join(".git").exists() {
        diagnostics.push(Diagnostic::new(
            "DSW-PROJECT-002",
            Severity::Pass,
            "git",
            "Git repository metadata is present.",
        ));
    } else {
        diagnostics.push(Diagnostic::new(
            "DSW-PROJECT-003",
            Severity::Notice,
            "git",
            "No .git metadata is present at the project root.",
        ));
    }

    inspect_toml_diagnostic(
        project.root(),
        ".demoswarm/config.toml",
        "DSW-CONFIG",
        false,
        &mut diagnostics,
    );
    inspect_toml_diagnostic(
        project.root(),
        ".demoswarm/state.toml",
        "DSW-STATE",
        true,
        &mut diagnostics,
    );

    let legacy = legacy_runtime_paths(project.root());
    if legacy.is_empty() {
        diagnostics.push(Diagnostic::new(
            "DSW-LEGACY-002",
            Severity::Pass,
            "legacy-runtime",
            "No known legacy runtime wrapper paths were found.",
        ));
    } else {
        diagnostics.push(
            Diagnostic::new(
                "DSW-LEGACY-001",
                Severity::Warning,
                "legacy-runtime",
                format!("found legacy runtime paths: {}", legacy.join(", ")),
            )
            .with_remediation("Migration support is required before safe removal.", false),
        );
    }

    let runs = scan_runs(project.root());
    match runs {
        Ok(items) => diagnostics.push(Diagnostic::new(
            "DSW-RUNS-001",
            Severity::Pass,
            ".runs",
            format!("{} run directories parsed sufficiently for inventory.", items.len()),
        )),
        Err(message) => diagnostics.push(Diagnostic::new(
            "DSW-RUNS-002",
            Severity::Error,
            ".runs",
            message,
        )),
    }

    let filter = PlatformFilter {
        platforms: args.platforms.clone(),
    };
    let platform_result = platforms(project, &filter, dry_run);
    if !platform_result.envelope.ok {
        diagnostics.extend(platform_result.envelope.diagnostics);
    }

    if args.fix {
        diagnostics.push(Diagnostic::new(
            "DSW-DOCTOR-001",
            Severity::Notice,
            "doctor-fix",
            "No ownership-proven automatic repair is available in the foundation release; no changes were made.",
        ));
    }

    let has_error = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error);
    let counts = diagnostic_counts(&diagnostics);
    let lines = vec![
        format!("Doctor: {}", if has_error { "FAILED" } else { "PASS" }),
        format!(
            "Diagnostics: {} error, {} warning, {} notice, {} pass",
            counts.0, counts.1, counts.2, counts.3
        ),
    ];
    let data = json!({
        "summary": {
            "errors": counts.0,
            "warnings": counts.1,
            "notices": counts.2,
            "passes": counts.3
        },
        "fix_requested": args.fix,
        "fixes_applied": 0
    });

    if has_error {
        CommandResult::failure(
            "doctor",
            dry_run,
            Some(project.display_root()),
            data,
            lines,
            diagnostics,
            EXIT_INVALID_STATE,
        )
    } else {
        CommandResult::success(
            "doctor",
            dry_run,
            Some(project.display_root()),
            data,
            lines,
            diagnostics,
        )
    }
}

fn runs(project: &ProjectContext, command: &RunsCommand, dry_run: bool) -> CommandResult {
    match command {
        RunsCommand::List => runs_list(project, dry_run),
        RunsCommand::Show { run_id } => runs_show(project, run_id, dry_run),
        RunsCommand::Validate { run_id } => runs_validate(project, run_id.as_deref(), dry_run),
        RunsCommand::RebuildIndex => unsupported(
            "runs rebuild-index",
            project,
            dry_run,
            "Index rebuilding will use the shared transaction engine so interrupted writes remain recoverable.",
        ),
        RunsCommand::Archive { .. } => unsupported(
            "runs archive",
            project,
            dry_run,
            "Run archiving requires the shared transaction engine and completion-policy checks.",
        ),
        RunsCommand::Export { .. } => unsupported(
            "runs export",
            project,
            dry_run,
            "Run export requires deterministic bundle creation and exact-surface secret scanning.",
        ),
    }
}

fn runs_list(project: &ProjectContext, dry_run: bool) -> CommandResult {
    match scan_runs(project.root()) {
        Ok(items) => {
            let mut lines = vec![format!("Runs: {}", items.len())];
            for item in &items {
                lines.push(format!(
                    "  {:<24} {:<12} {:<10} {:<10}",
                    item.run_id,
                    item.last_flow.as_deref().unwrap_or("-"),
                    item.completion.as_deref().unwrap_or("-"),
                    item.verification.as_deref().unwrap_or("-")
                ));
            }
            CommandResult::success(
                "runs list",
                dry_run,
                Some(project.display_root()),
                json!({ "runs": items }),
                lines,
                Vec::new(),
            )
        }
        Err(message) => {
            let diagnostic = Diagnostic::new(
                "DSW-RUNS-002",
                Severity::Error,
                ".runs",
                message,
            );
            CommandResult::failure(
                "runs list",
                dry_run,
                Some(project.display_root()),
                json!({ "runs": [] }),
                vec![diagnostic.message.clone()],
                vec![diagnostic],
                EXIT_INVALID_STATE,
            )
        }
    }
}

fn runs_show(project: &ProjectContext, run_id: &str, dry_run: bool) -> CommandResult {
    let run_dir = project.root().join(".runs").join(run_id);
    if !run_dir.is_dir() {
        let diagnostic = Diagnostic::new(
            "DSW-RUNS-003",
            Severity::Error,
            run_id,
            "run directory does not exist",
        );
        return CommandResult::failure(
            "runs show",
            dry_run,
            Some(project.display_root()),
            json!({ "run_id": run_id }),
            vec![diagnostic.message.clone()],
            vec![diagnostic],
            EXIT_INVALID_STATE,
        );
    }

    let manifest = read_run_manifest(&run_dir.join("run.json"));
    let mut flows = Vec::new();
    let mut lines = vec![format!("Run {run_id}")];
    for flow in FLOW_ORDER {
        let receipt_path = receipt_path(&run_dir, flow);
        if let Some(path) = receipt_path {
            match read_receipt(&path) {
                Ok(receipt) => {
                    let completion = receipt_completion(&receipt);
                    let verification = receipt_verification(&receipt);
                    lines.push(format!(
                        "  {:<8} {:<16} {}",
                        flow,
                        completion.as_deref().unwrap_or("unknown"),
                        verification.as_deref().unwrap_or("unknown")
                    ));
                    flows.push(json!({
                        "flow": flow,
                        "present": true,
                        "path": display_relative(project.root(), &path),
                        "completion": completion,
                        "verification": verification,
                        "generated_at": receipt.generated_at.or(receipt.completed_at),
                    }));
                }
                Err(error) => {
                    lines.push(format!("  {flow:<8} invalid receipt: {error}"));
                    flows.push(json!({
                        "flow": flow,
                        "present": true,
                        "path": display_relative(project.root(), &path),
                        "error": error,
                    }));
                }
            }
        } else {
            lines.push(format!("  {flow:<8} not present"));
            flows.push(json!({ "flow": flow, "present": false }));
        }
    }

    let manifest_value = match manifest {
        Ok(Some(value)) => json!(value),
        Ok(None) => Value::Null,
        Err(error) => json!({ "error": error }),
    };
    CommandResult::success(
        "runs show",
        dry_run,
        Some(project.display_root()),
        json!({
            "run_id": run_id,
            "manifest": manifest_value,
            "flows": flows
        }),
        lines,
        Vec::new(),
    )
}

fn runs_validate(project: &ProjectContext, run_id: Option<&str>, dry_run: bool) -> CommandResult {
    let run_ids = if let Some(run_id) = run_id {
        vec![run_id.to_string()]
    } else {
        match discover_run_ids(project.root()) {
            Ok(ids) => ids,
            Err(message) => {
                let diagnostic = Diagnostic::new(
                    "DSW-RUNS-002",
                    Severity::Error,
                    ".runs",
                    message,
                );
                return CommandResult::failure(
                    "runs validate",
                    dry_run,
                    Some(project.display_root()),
                    json!({ "runs": [] }),
                    vec![diagnostic.message.clone()],
                    vec![diagnostic],
                    EXIT_INVALID_STATE,
                );
            }
        }
    };

    let mut diagnostics = Vec::new();
    let mut validated = Vec::new();
    for id in run_ids {
        let run_dir = project.root().join(".runs").join(&id);
        if !run_dir.is_dir() {
            diagnostics.push(Diagnostic::new(
                "DSW-RUNS-003",
                Severity::Error,
                id.clone(),
                "run directory does not exist",
            ));
            continue;
        }

        match read_run_manifest(&run_dir.join("run.json")) {
            Ok(Some(manifest)) => {
                if manifest.run_id != id {
                    diagnostics.push(Diagnostic::new(
                        "DSW-RUNS-004",
                        Severity::Error,
                        id.clone(),
                        format!(
                            "run.json identifies `{}` instead of directory `{id}`",
                            manifest.run_id
                        ),
                    ));
                }
            }
            Ok(None) => diagnostics.push(Diagnostic::new(
                "DSW-RUNS-005",
                Severity::Notice,
                id.clone(),
                "run.json is absent; this may be a legacy run",
            )),
            Err(error) => diagnostics.push(Diagnostic::new(
                "DSW-RUNS-006",
                Severity::Error,
                id.clone(),
                format!("run.json is invalid: {error}"),
            )),
        }

        let mut receipt_count = 0usize;
        for flow in FLOW_ORDER {
            let Some(path) = receipt_path(&run_dir, flow) else {
                continue;
            };
            receipt_count += 1;
            match read_receipt(&path) {
                Ok(receipt) => {
                    if receipt.run_id != id {
                        diagnostics.push(Diagnostic::new(
                            "DSW-RECEIPT-001",
                            Severity::Error,
                            display_relative(project.root(), &path),
                            format!("receipt run_id `{}` does not match `{id}`", receipt.run_id),
                        ));
                    }
                    if receipt.flow != flow {
                        diagnostics.push(Diagnostic::new(
                            "DSW-RECEIPT-002",
                            Severity::Error,
                            display_relative(project.root(), &path),
                            format!("receipt flow `{}` does not match `{flow}`", receipt.flow),
                        ));
                    }
                }
                Err(error) => diagnostics.push(Diagnostic::new(
                    "DSW-RECEIPT-003",
                    Severity::Error,
                    display_relative(project.root(), &path),
                    format!("receipt is invalid JSON: {error}"),
                )),
            }
        }
        validated.push(json!({ "run_id": id, "receipts": receipt_count }));
    }

    let errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .count();
    let lines = vec![format!(
        "Validated {} run(s): {errors} error(s)",
        validated.len()
    )];
    let data = json!({ "runs": validated, "errors": errors });
    if errors == 0 {
        CommandResult::success(
            "runs validate",
            dry_run,
            Some(project.display_root()),
            data,
            lines,
            diagnostics,
        )
    } else {
        CommandResult::failure(
            "runs validate",
            dry_run,
            Some(project.display_root()),
            data,
            lines,
            diagnostics,
            EXIT_INVALID_STATE,
        )
    }
}

fn unsupported(command: &str, project: &ProjectContext, dry_run: bool, message: &str) -> CommandResult {
    let diagnostic = Diagnostic::new(
        "DSW-COMMAND-001",
        Severity::Error,
        command,
        message,
    );
    CommandResult::failure(
        command,
        dry_run,
        Some(project.display_root()),
        json!({ "implemented": false, "side_effects_performed": false }),
        vec![message.to_string()],
        vec![diagnostic],
        EXIT_UNSUPPORTED,
    )
}

fn platform_definitions() -> Vec<PlatformStatus> {
    vec![
        PlatformStatus {
            id: "claude-code",
            display_name: "Claude Code",
            support: "preview",
            executable: "claude",
            detected: false,
            executable_path: None,
            project_markers: Vec::new(),
        },
        PlatformStatus {
            id: "codex",
            display_name: "Codex",
            support: "experimental",
            executable: "codex",
            detected: false,
            executable_path: None,
            project_markers: Vec::new(),
        },
        PlatformStatus {
            id: "gemini-cli",
            display_name: "Gemini CLI",
            support: "experimental",
            executable: "gemini",
            detected: false,
            executable_path: None,
            project_markers: Vec::new(),
        },
        PlatformStatus {
            id: "openclaw",
            display_name: "OpenClaw",
            support: "experimental",
            executable: "openclaw",
            detected: false,
            executable_path: None,
            project_markers: Vec::new(),
        },
    ]
}

fn detect_platforms(project: &Path) -> Vec<PlatformStatus> {
    let mut platforms = platform_definitions();
    for platform in &mut platforms {
        platform.executable_path = find_executable(platform.executable)
            .map(|path| path.to_string_lossy().into_owned());
        platform.project_markers = project_markers(project, platform.id);
        platform.detected = platform.executable_path.is_some() || !platform.project_markers.is_empty();
    }
    platforms
}

fn project_markers(project: &Path, id: &str) -> Vec<String> {
    let candidates: &[&str] = match id {
        "claude-code" => &[".claude"],
        "codex" => &["AGENTS.md", ".codex"],
        "gemini-cli" => &[".gemini"],
        "openclaw" => &[".openclaw"],
        _ => &[],
    };
    candidates
        .iter()
        .filter(|candidate| project.join(candidate).exists())
        .map(|candidate| (*candidate).to_string())
        .collect()
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    let extensions: Vec<OsString> = if cfg!(windows) {
        std::env::var_os("PATHEXT")
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .filter(|extension| !extension.is_empty())
                    .map(OsString::from)
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    OsString::from(".COM"),
                    OsString::from(".EXE"),
                    OsString::from(".BAT"),
                    OsString::from(".CMD"),
                ]
            })
    } else {
        vec![OsString::new()]
    };

    for directory in std::env::split_paths(&paths) {
        for extension in &extensions {
            let mut file_name = OsString::from(name);
            file_name.push(extension);
            let candidate = directory.join(file_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn inspect_toml(path: &Path) -> (String, Option<String>) {
    if !path.exists() {
        return ("absent".to_string(), None);
    }
    match fs::read_to_string(path) {
        Ok(content) => match toml::from_str::<toml::Value>(&content) {
            Ok(_) => ("valid".to_string(), None),
            Err(error) => (
                "invalid".to_string(),
                Some(format!("{} is invalid TOML: {error}", path.display())),
            ),
        },
        Err(error) => (
            "unreadable".to_string(),
            Some(format!("could not read {}: {error}", path.display())),
        ),
    }
}

fn inspect_toml_diagnostic(
    project: &Path,
    relative: &str,
    code_prefix: &str,
    optional: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let path = project.join(relative);
    let (status, error) = inspect_toml(&path);
    match (status.as_str(), error) {
        ("valid", _) => diagnostics.push(Diagnostic::new(
            format!("{code_prefix}-001"),
            Severity::Pass,
            relative,
            "file parses successfully",
        )),
        ("absent", _) if optional => diagnostics.push(Diagnostic::new(
            format!("{code_prefix}-003"),
            Severity::Notice,
            relative,
            "file is absent",
        )),
        ("absent", _) => diagnostics.push(
            Diagnostic::new(
                format!("{code_prefix}-003"),
                Severity::Notice,
                relative,
                "file is absent",
            )
            .with_remediation("Run `demoswarm configure` to create project intent.", true),
        ),
        (_, Some(message)) => diagnostics.push(Diagnostic::new(
            format!("{code_prefix}-002"),
            Severity::Error,
            relative,
            message,
        )),
        _ => diagnostics.push(Diagnostic::new(
            format!("{code_prefix}-004"),
            Severity::Error,
            relative,
            "file state could not be classified",
        )),
    }
}

fn diagnostic_counts(diagnostics: &[Diagnostic]) -> (usize, usize, usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    let mut notices = 0;
    let mut passes = 0;
    for diagnostic in diagnostics {
        match diagnostic.severity {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
            Severity::Notice => notices += 1,
            Severity::Pass => passes += 1,
        }
    }
    (errors, warnings, notices, passes)
}

fn legacy_runtime_paths(project: &Path) -> Vec<String> {
    [
        ".claude/scripts/demoswarm.sh",
        ".claude/scripts/pack-check.sh",
        ".demoswarm/bin/demoswarm",
        ".demoswarm/bin/demoswarm.exe",
        "tools/demoswarm-runs-tools",
        "tools/demoswarm-pack-check",
        "scripts/runs_tools.py",
    ]
    .iter()
    .filter(|relative| project.join(relative).exists())
    .map(|relative| (*relative).to_string())
    .collect()
}

fn detect_repository_provider(project: &Path) -> String {
    let config_path = project.join(".git/config");
    match fs::read_to_string(config_path) {
        Ok(content) if content.contains("github.com") => "github".to_string(),
        Ok(content) if content.contains("gitlab.com") => "gitlab".to_string(),
        _ => "unknown".to_string(),
    }
}

fn discover_run_ids(project: &Path) -> Result<Vec<String>, String> {
    let runs_dir = project.join(".runs");
    if !runs_dir.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&runs_dir)
        .map_err(|error| format!("could not read {}: {error}", runs_dir.display()))?;
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read run entry: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| format!("could not inspect run entry: {error}"))?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('_') || name == "archive" {
            continue;
        }
        ids.push(name);
    }
    ids.sort();
    Ok(ids)
}

fn scan_runs(project: &Path) -> Result<Vec<RunSummary>, String> {
    let ids = discover_run_ids(project)?;
    let mut summaries = Vec::new();
    for id in ids {
        let run_dir = project.join(".runs").join(&id);
        let manifest = read_run_manifest(&run_dir.join("run.json"));
        let (host, adapter_version, manifest_updated, legacy) = match manifest {
            Ok(Some(manifest)) => (
                manifest.producer.as_ref().map(|producer| producer.host.clone()),
                manifest
                    .producer
                    .and_then(|producer| producer.adapter_version),
                manifest.updated_at,
                false,
            ),
            Ok(None) => (None, None, None, true),
            Err(_) => (None, None, None, false),
        };

        let mut last_receipt = None;
        for flow in FLOW_ORDER {
            let Some(path) = receipt_path(&run_dir, flow) else {
                continue;
            };
            if let Ok(receipt) = read_receipt(&path) {
                last_receipt = Some(receipt);
            }
        }
        let last_flow = last_receipt.as_ref().map(|receipt| receipt.flow.clone());
        let completion = last_receipt.as_ref().and_then(receipt_completion);
        let verification = last_receipt.as_ref().and_then(receipt_verification);
        let receipt_updated = last_receipt
            .and_then(|receipt| receipt.generated_at.or(receipt.completed_at));

        summaries.push(RunSummary {
            run_id: id,
            host,
            adapter_version,
            last_flow,
            completion,
            verification,
            updated_at: receipt_updated.or(manifest_updated),
            legacy,
        });
    }
    Ok(summaries)
}

fn read_run_manifest(path: &Path) -> Result<Option<RunManifest>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))
}

fn receipt_path(run_dir: &Path, flow: &str) -> Option<PathBuf> {
    let v2 = run_dir.join(flow).join("receipt.json");
    if v2.is_file() {
        return Some(v2);
    }
    let legacy = run_dir.join(flow).join(format!("{flow}_receipt.json"));
    legacy.is_file().then_some(legacy)
}

fn read_receipt(path: &Path) -> Result<Receipt, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_str(&content).map_err(|error| error.to_string())
}

fn receipt_completion(receipt: &Receipt) -> Option<String> {
    receipt.completion.clone().or_else(|| {
        receipt.status.as_deref().map(|status| match status {
            "PARTIAL" => "PARTIAL".to_string(),
            "CANNOT_PROCEED" => "CANNOT_PROCEED".to_string(),
            _ => "COMPLETE".to_string(),
        })
    })
}

fn receipt_verification(receipt: &Receipt) -> Option<String> {
    receipt.verification.clone().or_else(|| {
        receipt.status.as_deref().map(|status| match status {
            "VERIFIED" => "VERIFIED".to_string(),
            _ => "UNVERIFIED".to_string(),
        })
    })
}

fn display_relative(project: &Path, path: &Path) -> String {
    path.strip_prefix(project)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
