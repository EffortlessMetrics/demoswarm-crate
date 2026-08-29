use demoswarm::transaction::{
    ExecutionOptions, ExecutionOutcome, FailurePoint, LastTransaction, ManagerState, Operation,
    OperationPlan, Ownership, StatePrecondition, TransactionError, TransactionExecutor,
    TransactionPhase, read_latest_journal, sha256_bytes,
};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

fn operation_plan(root: &Path, transaction_id: &str, operations: Vec<Operation>) -> OperationPlan {
    OperationPlan::new(transaction_id, "install", root.to_path_buf(), operations)
        .expect("valid operation plan")
}

fn desired_state(transaction_id: &str) -> ManagerState {
    ManagerState {
        schema_version: 1,
        generation: 1,
        manager_version: env!("CARGO_PKG_VERSION").to_string(),
        pack: None,
        adapters: Vec::new(),
        managed_paths: Vec::new(),
        last_transaction: LastTransaction {
            id: transaction_id.to_string(),
            command: "install".to_string(),
            status: "committed".to_string(),
        },
    }
}

fn create_file_plan(root: &Path, transaction_id: &str) -> OperationPlan {
    operation_plan(
        root,
        transaction_id,
        vec![
            Operation::CreateDirectory {
                path: PathBuf::from("managed"),
                ownership: Ownership::owned("test-adapter"),
            },
            Operation::CreateFile {
                path: PathBuf::from("managed/value.txt"),
                content: "created\n".to_string(),
                ownership: Ownership::owned("test-adapter"),
            },
        ],
    )
}

#[test]
fn plan_rejects_path_traversal() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let result = OperationPlan::new(
        "tx-path",
        "install",
        temporary.path().to_path_buf(),
        vec![Operation::CreateFile {
            path: PathBuf::from("../escape.txt"),
            content: "nope".to_string(),
            ownership: Ownership::owned("test-adapter"),
        }],
    );
    assert!(matches!(result, Err(TransactionError::InvalidPlan(_))));
}

#[test]
fn plan_rejects_duplicate_whole_file_targets() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let result = OperationPlan::new(
        "tx-duplicate",
        "install",
        temporary.path().to_path_buf(),
        vec![
            Operation::CreateFile {
                path: PathBuf::from("same.txt"),
                content: "one".to_string(),
                ownership: Ownership::owned("one"),
            },
            Operation::CreateFile {
                path: PathBuf::from("same.txt"),
                content: "two".to_string(),
                ownership: Ownership::owned("two"),
            },
        ],
    );
    assert!(matches!(result, Err(TransactionError::InvalidPlan(_))));
}

#[test]
fn plan_digest_detects_serialized_tampering() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let plan = create_file_plan(temporary.path(), "tx-digest");
    let mut value = serde_json::to_value(&plan).expect("serialize plan");
    value["command"] = serde_json::Value::String("remove".to_string());
    let tampered: OperationPlan = serde_json::from_value(value).expect("deserialize plan");
    assert!(matches!(
        tampered.validate(),
        Err(TransactionError::InvalidPlan(_))
    ));
}

#[test]
fn dry_run_validates_without_writing() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let plan = create_file_plan(temporary.path(), "tx-dry-run");
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let report = executor
        .execute(
            &plan,
            ExecutionOptions {
                dry_run: true,
                ..ExecutionOptions::default()
            },
        )
        .expect("dry run succeeds");

    assert_eq!(report.outcome, ExecutionOutcome::DryRun);
    assert!(!report.side_effects_performed);
    assert!(!temporary.path().join("managed").exists());
    assert!(!temporary.path().join(".demoswarm").exists());
}

#[test]
fn create_transaction_commits_files_and_append_only_journal() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let plan = create_file_plan(temporary.path(), "tx-create");
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let report = executor
        .execute(&plan, ExecutionOptions::default())
        .expect("transaction commits");

    assert_eq!(report.outcome, ExecutionOutcome::Committed);
    assert!(report.side_effects_performed);
    assert_eq!(
        fs::read_to_string(temporary.path().join("managed/value.txt"))
            .expect("managed file readable"),
        "created\n"
    );
    let journal_dir = report.journal_dir.expect("journal directory");
    let journal = read_latest_journal(&journal_dir).expect("latest journal");
    assert_eq!(journal.phase, TransactionPhase::Committed);
    assert!(journal.sequence >= 5);
    assert!(journal_dir.join("receipt.json").is_file());
}

#[test]
fn replace_and_remove_require_and_preserve_exact_preconditions() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let managed = temporary.path().join("managed");
    fs::create_dir(&managed).expect("managed directory");
    fs::write(managed.join("replace.txt"), "old\n").expect("replacement source");
    fs::write(managed.join("remove.txt"), "remove\n").expect("removal source");

    let plan = operation_plan(
        temporary.path(),
        "tx-replace-remove",
        vec![
            Operation::ReplaceOwnedFile {
                path: PathBuf::from("managed/replace.txt"),
                content: "new\n".to_string(),
                expected_sha256: sha256_bytes(b"old\n"),
                ownership: Ownership::owned("test-adapter"),
            },
            Operation::RemoveOwnedFile {
                path: PathBuf::from("managed/remove.txt"),
                expected_sha256: sha256_bytes(b"remove\n"),
                ownership: Ownership::owned("test-adapter"),
            },
        ],
    );
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    executor
        .execute(&plan, ExecutionOptions::default())
        .expect("transaction commits");

    assert_eq!(
        fs::read_to_string(managed.join("replace.txt")).expect("replacement readable"),
        "new\n"
    );
    assert!(!managed.join("remove.txt").exists());
}

#[test]
fn precondition_drift_aborts_before_transaction_infrastructure() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    fs::write(temporary.path().join("value.txt"), "old").expect("source written");
    let plan = operation_plan(
        temporary.path(),
        "tx-drift",
        vec![Operation::ReplaceOwnedFile {
            path: PathBuf::from("value.txt"),
            content: "new".to_string(),
            expected_sha256: sha256_bytes(b"old"),
            ownership: Ownership::owned("test-adapter"),
        }],
    );
    fs::write(temporary.path().join("value.txt"), "changed").expect("drift written");
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let result = executor.execute(&plan, ExecutionOptions::default());

    assert!(matches!(result, Err(TransactionError::Precondition(_))));
    assert_eq!(
        fs::read_to_string(temporary.path().join("value.txt")).expect("value readable"),
        "changed"
    );
    assert!(!temporary.path().join(".demoswarm").exists());
}

#[test]
fn injected_failure_rolls_back_created_file_and_directory() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let plan = create_file_plan(temporary.path(), "tx-rollback");
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let result = executor.execute(
        &plan,
        ExecutionOptions {
            failure_point: Some(FailurePoint::AfterOperation(2)),
            ..ExecutionOptions::default()
        },
    );

    match result {
        Err(TransactionError::ExecutionFailed {
            recovery_required,
            journal_dir,
            ..
        }) => {
            assert!(!recovery_required);
            let journal = read_latest_journal(&journal_dir).expect("latest journal");
            assert_eq!(journal.phase, TransactionPhase::RolledBack);
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert!(!temporary.path().join("managed/value.txt").exists());
    assert!(!temporary.path().join("managed").exists());
}

#[test]
fn injected_rollback_failure_leaves_recovery_journal() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let plan = create_file_plan(temporary.path(), "tx-recovery");
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let result = executor.execute(
        &plan,
        ExecutionOptions {
            failure_point: Some(FailurePoint::AfterOperation(2)),
            rollback_failure_after: Some(0),
            dry_run: false,
        },
    );

    match result {
        Err(TransactionError::ExecutionFailed {
            recovery_required,
            journal_dir,
            ..
        }) => {
            assert!(recovery_required);
            let journal = read_latest_journal(&journal_dir).expect("latest journal");
            assert_eq!(journal.phase, TransactionPhase::RecoveryRequired);
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert!(temporary.path().join("managed/value.txt").is_file());
}

#[test]
fn state_is_written_only_after_payload_validation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let transaction_id = "tx-state-rollback";
    let plan = create_file_plan(temporary.path(), transaction_id)
        .with_state(desired_state(transaction_id), StatePrecondition::Absent)
        .expect("plan with state");
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let result = executor.execute(
        &plan,
        ExecutionOptions {
            failure_point: Some(FailurePoint::BeforeStateWrite),
            ..ExecutionOptions::default()
        },
    );

    assert!(matches!(
        result,
        Err(TransactionError::ExecutionFailed {
            recovery_required: false,
            ..
        })
    ));
    assert!(!temporary.path().join("managed").exists());
    assert!(!temporary.path().join(".demoswarm/state.toml").exists());
}

#[test]
fn successful_state_write_records_last_transaction() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let transaction_id = "tx-state-commit";
    let plan = create_file_plan(temporary.path(), transaction_id)
        .with_state(desired_state(transaction_id), StatePrecondition::Absent)
        .expect("plan with state");
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let report = executor
        .execute(&plan, ExecutionOptions::default())
        .expect("transaction commits");

    assert!(report.state_written);
    let state_content = fs::read_to_string(temporary.path().join(".demoswarm/state.toml"))
        .expect("state readable");
    let state: ManagerState = toml::from_str(&state_content).expect("state parses");
    assert_eq!(state.last_transaction.id, transaction_id);
    assert_eq!(state.generation, 1);
}

#[test]
fn held_project_lock_rejects_concurrent_mutation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let manager = temporary.path().join(".demoswarm");
    fs::create_dir(&manager).expect("manager directory");
    let lock_path = manager.join("transaction.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .expect("lock file");
    lock.try_lock().expect("test owns lock");

    let plan = create_file_plan(temporary.path(), "tx-concurrent");
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let result = executor.execute(&plan, ExecutionOptions::default());
    assert!(matches!(result, Err(TransactionError::Concurrent(_))));
    assert!(!temporary.path().join("managed").exists());
    drop(lock);
}

#[test]
fn native_package_operations_are_previewable_but_not_executed() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let plan = operation_plan(
        temporary.path(),
        "tx-native-preview",
        vec![Operation::NativePackageInstall {
            adapter: "claude-code".to_string(),
            identity: "demoswarm".to_string(),
            program: "claude".to_string(),
            args: vec!["plugin".to_string(), "install".to_string()],
            compensation: None,
        }],
    );
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let preview = executor
        .execute(
            &plan,
            ExecutionOptions {
                dry_run: true,
                ..ExecutionOptions::default()
            },
        )
        .expect("native operation can be previewed");
    assert_eq!(preview.outcome, ExecutionOutcome::DryRun);

    let result = executor.execute(&plan, ExecutionOptions::default());
    assert!(matches!(result, Err(TransactionError::Unsupported(_))));
    assert!(!temporary.path().join(".demoswarm").exists());
}

#[cfg(unix)]
#[test]
fn executor_refuses_symlink_ancestors() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let outside = tempfile::tempdir().expect("outside directory");
    symlink(outside.path(), temporary.path().join("linked")).expect("symlink created");
    let plan = operation_plan(
        temporary.path(),
        "tx-symlink",
        vec![Operation::CreateFile {
            path: PathBuf::from("linked/value.txt"),
            content: "blocked".to_string(),
            ownership: Ownership::owned("test-adapter"),
        }],
    );
    let executor = TransactionExecutor::new(temporary.path()).expect("executor");
    let result = executor.execute(&plan, ExecutionOptions::default());
    assert!(matches!(result, Err(TransactionError::Precondition(_))));
    assert!(!outside.path().join("value.txt").exists());
}
