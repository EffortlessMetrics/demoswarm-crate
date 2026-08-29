use super::{
    JournalOperationStatus, ManagerState, Operation, OperationPlan, StatePrecondition,
    TransactionError, TransactionJournal, TransactionPhase, TransactionReceipt, sha256_bytes,
    sha256_file,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Deterministic failure points used by transaction recovery tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePoint {
    BeforeFirstOperation,
    AfterOperation(usize),
    BeforeStateWrite,
}

/// Controls execution without changing the authenticated plan.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecutionOptions {
    pub dry_run: bool,
    pub failure_point: Option<FailurePoint>,
    /// Test-style injection used to prove recovery-required journals.
    pub rollback_failure_after: Option<usize>,
}

/// Terminal result of an execution attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    DryRun,
    Committed,
}

/// Stable execution summary returned to lifecycle commands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionReport {
    pub transaction_id: String,
    pub plan_digest: String,
    pub outcome: ExecutionOutcome,
    pub operations_planned: usize,
    pub operations_applied: usize,
    pub state_written: bool,
    pub side_effects_performed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_dir: Option<PathBuf>,
}

/// Executes typed transaction plans inside one canonical project root.
#[derive(Debug, Clone)]
pub struct TransactionExecutor {
    project_root: PathBuf,
}

impl TransactionExecutor {
    pub fn new(project_root: impl AsRef<Path>) -> Result<Self, TransactionError> {
        let project_root = project_root.as_ref();
        if !project_root.is_dir() {
            return Err(TransactionError::Precondition(format!(
                "project root is not a directory: {}",
                project_root.display()
            )));
        }
        let project_root = fs::canonicalize(project_root).map_err(|error| {
            TransactionError::io("canonicalize project root", project_root, error)
        })?;
        Ok(Self { project_root })
    }

    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn execute(
        &self,
        plan: &OperationPlan,
        options: ExecutionOptions,
    ) -> Result<ExecutionReport, TransactionError> {
        plan.validate()?;
        self.validate_project_identity(plan)?;
        self.preflight(plan, options.dry_run)?;

        if options.dry_run {
            return Ok(ExecutionReport {
                transaction_id: plan.transaction_id().to_string(),
                plan_digest: plan.plan_digest().to_string(),
                outcome: ExecutionOutcome::DryRun,
                operations_planned: plan.operations().len(),
                operations_applied: 0,
                state_written: false,
                side_effects_performed: false,
                journal_dir: None,
            });
        }

        self.reject_unimplemented_operations(plan)?;
        let manager_dir = self.prepare_manager_directory()?;
        let _lock = ProjectLock::acquire(&manager_dir, plan.transaction_id())?;

        // Recheck after taking the lock. A preview or another process may have
        // changed the project between initial planning and mutation authority.
        self.preflight(plan, false)?;
        let operations_dir = self.prepare_operations_directory(&manager_dir)?;

        let journal_dir = operations_dir.join(plan.transaction_id());
        if journal_dir.exists() {
            return Err(TransactionError::Precondition(format!(
                "transaction journal already exists: {}",
                journal_dir.display()
            )));
        }
        let backup_dir = journal_dir.join("backups");
        fs::create_dir_all(&backup_dir).map_err(|error| {
            TransactionError::io("create transaction journal directories", &journal_dir, error)
        })?;

        let mut journal = TransactionJournal::new(plan);
        journal.persist(&journal_dir)?;
        let mut applied = Vec::new();

        if options.failure_point == Some(FailurePoint::BeforeFirstOperation) {
            return Err(self.fail_transaction(
                "injected failure before first operation".to_string(),
                &journal_dir,
                &mut journal,
                &mut applied,
                options,
            ));
        }

        journal.phase = TransactionPhase::Applying;
        if let Err(error) = journal.persist(&journal_dir) {
            return Err(self.fail_transaction(
                error.to_string(),
                &journal_dir,
                &mut journal,
                &mut applied,
                options,
            ));
        }

        for (index, operation) in plan.operations().iter().enumerate() {
            let backup_relative = backup_relative_path(plan.transaction_id(), index, operation);
            if let Some(path) = &backup_relative {
                match journal.operation_mut(index) {
                    Ok(entry) => entry.backup_path = Some(path.clone()),
                    Err(error) => {
                        return Err(self.fail_transaction(
                            error.to_string(),
                            &journal_dir,
                            &mut journal,
                            &mut applied,
                            options,
                        ));
                    }
                }
                if let Err(error) = journal.persist(&journal_dir) {
                    return Err(self.fail_transaction(
                        error.to_string(),
                        &journal_dir,
                        &mut journal,
                        &mut applied,
                        options,
                    ));
                }
            }

            match self.apply_operation(
                operation,
                index,
                plan.transaction_id(),
                &backup_dir,
                &mut applied,
            ) {
                Ok(record) => {
                    let entry = match journal.operation_mut(index) {
                        Ok(entry) => entry,
                        Err(error) => {
                            return Err(self.fail_transaction(
                                error.to_string(),
                                &journal_dir,
                                &mut journal,
                                &mut applied,
                                options,
                            ));
                        }
                    };
                    entry.status = JournalOperationStatus::Applied;
                    entry.before_sha256 = record.before_sha256;
                    entry.after_sha256 = record.after_sha256;
                    if let Err(error) = journal.persist(&journal_dir) {
                        return Err(self.fail_transaction(
                            error.to_string(),
                            &journal_dir,
                            &mut journal,
                            &mut applied,
                            options,
                        ));
                    }
                }
                Err(error) => {
                    return Err(self.fail_transaction(
                        error.to_string(),
                        &journal_dir,
                        &mut journal,
                        &mut applied,
                        options,
                    ));
                }
            }

            if options.failure_point == Some(FailurePoint::AfterOperation(index + 1)) {
                return Err(self.fail_transaction(
                    format!("injected failure after operation {}", index + 1),
                    &journal_dir,
                    &mut journal,
                    &mut applied,
                    options,
                ));
            }
        }

        journal.phase = TransactionPhase::Validating;
        if let Err(error) = journal.persist(&journal_dir) {
            return Err(self.fail_transaction(
                error.to_string(),
                &journal_dir,
                &mut journal,
                &mut applied,
                options,
            ));
        }
        if let Err(error) = self.post_validate(plan) {
            return Err(self.fail_transaction(
                error.to_string(),
                &journal_dir,
                &mut journal,
                &mut applied,
                options,
            ));
        }

        if plan.desired_state().is_some() {
            if options.failure_point == Some(FailurePoint::BeforeStateWrite) {
                return Err(self.fail_transaction(
                    "injected failure before state write".to_string(),
                    &journal_dir,
                    &mut journal,
                    &mut applied,
                    options,
                ));
            }
            journal.phase = TransactionPhase::WritingState;
            if let Err(error) = journal.persist(&journal_dir) {
                return Err(self.fail_transaction(
                    error.to_string(),
                    &journal_dir,
                    &mut journal,
                    &mut applied,
                    options,
                ));
            }
            if let Err(error) = self.write_state(plan, &backup_dir, &mut applied) {
                return Err(self.fail_transaction(
                    error.to_string(),
                    &journal_dir,
                    &mut journal,
                    &mut applied,
                    options,
                ));
            }
            journal.state_written = true;
            if let Err(error) = journal.persist(&journal_dir) {
                return Err(self.fail_transaction(
                    error.to_string(),
                    &journal_dir,
                    &mut journal,
                    &mut applied,
                    options,
                ));
            }
        }

        journal.phase = TransactionPhase::Committed;
        journal.error = None;
        journal.recovery_required = false;
        journal.persist(&journal_dir)?;
        TransactionReceipt::from_journal(&journal).persist(&journal_dir)?;

        Ok(ExecutionReport {
            transaction_id: plan.transaction_id().to_string(),
            plan_digest: plan.plan_digest().to_string(),
            outcome: ExecutionOutcome::Committed,
            operations_planned: plan.operations().len(),
            operations_applied: plan.operations().len(),
            state_written: journal.state_written,
            side_effects_performed: true,
            journal_dir: Some(journal_dir),
        })
    }

    fn validate_project_identity(&self, plan: &OperationPlan) -> Result<(), TransactionError> {
        let planned_root = fs::canonicalize(plan.project_root()).map_err(|error| {
            TransactionError::io(
                "canonicalize planned project root",
                plan.project_root(),
                error,
            )
        })?;
        if planned_root != self.project_root {
            return Err(TransactionError::InvalidPlan(format!(
                "plan project root {} does not match executor root {}",
                planned_root.display(),
                self.project_root.display()
            )));
        }
        Ok(())
    }

    fn prepare_manager_directory(&self) -> Result<PathBuf, TransactionError> {
        let manager_dir = self.project_root.join(".demoswarm");
        if manager_dir.exists() {
            ensure_safe_existing_path(&manager_dir)?;
            if !manager_dir.is_dir() {
                return Err(TransactionError::Precondition(format!(
                    "manager state path is not a directory: {}",
                    manager_dir.display()
                )));
            }
        } else {
            fs::create_dir(&manager_dir).map_err(|error| {
                TransactionError::io("create manager state directory", &manager_dir, error)
            })?;
        }
        Ok(manager_dir)
    }

    fn prepare_operations_directory(
        &self,
        manager_dir: &Path,
    ) -> Result<PathBuf, TransactionError> {
        let operations_dir = manager_dir.join("operations");
        if operations_dir.exists() {
            ensure_safe_existing_path(&operations_dir)?;
            if !operations_dir.is_dir() {
                return Err(TransactionError::Precondition(format!(
                    "transaction operations path is not a directory: {}",
                    operations_dir.display()
                )));
            }
        } else {
            fs::create_dir(&operations_dir).map_err(|error| {
                TransactionError::io("create operations directory", &operations_dir, error)
            })?;
        }
        Ok(operations_dir)
    }

    fn preflight(
        &self,
        plan: &OperationPlan,
        allow_unimplemented: bool,
    ) -> Result<(), TransactionError> {
        let mut planned_directories = BTreeSet::from([PathBuf::new()]);
        for operation in plan.operations() {
            match operation {
                Operation::CreateDirectory { path, .. } => {
                    self.check_path_safety(path)?;
                    self.require_parent_directory(path, &planned_directories)?;
                    let target = self.project_root.join(path);
                    if target.exists() {
                        ensure_safe_existing_path(&target)?;
                        if !target.is_dir() {
                            return Err(TransactionError::Precondition(format!(
                                "directory target exists but is not a directory: {}",
                                target.display()
                            )));
                        }
                    }
                    planned_directories.insert(path.clone());
                }
                Operation::CreateFile { path, .. } => {
                    self.check_path_safety(path)?;
                    self.require_parent_directory(path, &planned_directories)?;
                    let target = self.project_root.join(path);
                    if target.exists() {
                        return Err(TransactionError::Precondition(format!(
                            "create target already exists: {}",
                            target.display()
                        )));
                    }
                }
                Operation::ReplaceOwnedFile {
                    path,
                    expected_sha256,
                    ..
                }
                | Operation::RemoveOwnedFile {
                    path,
                    expected_sha256,
                    ..
                } => {
                    self.check_path_safety(path)?;
                    self.require_parent_directory(path, &planned_directories)?;
                    let target = self.project_root.join(path);
                    ensure_regular_file(&target)?;
                    let observed = sha256_file(&target)?;
                    if observed != *expected_sha256 {
                        return Err(TransactionError::Precondition(format!(
                            "{} changed after planning: expected {expected_sha256}, observed {observed}",
                            path.display()
                        )));
                    }
                }
                Operation::MergeJson { path, .. } | Operation::MergeToml { path, .. } => {
                    self.check_path_safety(path)?;
                    self.require_parent_directory(path, &planned_directories)?;
                    if !allow_unimplemented {
                        return Err(TransactionError::Unsupported(format!(
                            "{} execution is reserved for the semantic merge slice",
                            operation.kind()
                        )));
                    }
                }
                Operation::NativePackageInstall { .. }
                | Operation::NativePackageUpdate { .. }
                | Operation::NativePackageRemove { .. } => {
                    if !allow_unimplemented {
                        return Err(TransactionError::Unsupported(format!(
                            "{} execution is reserved for host adapter issue #5",
                            operation.kind()
                        )));
                    }
                }
            }
        }
        self.check_state_precondition(plan)?;
        Ok(())
    }

    fn reject_unimplemented_operations(
        &self,
        plan: &OperationPlan,
    ) -> Result<(), TransactionError> {
        if let Some(operation) = plan.operations().iter().find(|operation| {
            matches!(
                operation,
                Operation::MergeJson { .. }
                    | Operation::MergeToml { .. }
                    | Operation::NativePackageInstall { .. }
                    | Operation::NativePackageUpdate { .. }
                    | Operation::NativePackageRemove { .. }
            )
        }) {
            return Err(TransactionError::Unsupported(format!(
                "{} has no executor in the transaction-core slice",
                operation.kind()
            )));
        }
        Ok(())
    }

    fn check_path_safety(&self, relative: &Path) -> Result<(), TransactionError> {
        let mut current = self.project_root.clone();
        for component in relative.components() {
            current.push(component.as_os_str());
            if !current.exists() {
                break;
            }
            ensure_safe_existing_path(&current)?;
        }
        Ok(())
    }

    fn require_parent_directory(
        &self,
        relative: &Path,
        planned_directories: &BTreeSet<PathBuf>,
    ) -> Result<(), TransactionError> {
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        if planned_directories.contains(parent) {
            return Ok(());
        }
        let absolute = self.project_root.join(parent);
        ensure_safe_existing_path(&absolute)?;
        if !absolute.is_dir() {
            return Err(TransactionError::Precondition(format!(
                "operation parent is not a directory: {}",
                absolute.display()
            )));
        }
        Ok(())
    }

    fn check_state_precondition(&self, plan: &OperationPlan) -> Result<(), TransactionError> {
        let Some(precondition) = plan.state_precondition() else {
            return Ok(());
        };
        let state_path = self.project_root.join(".demoswarm/state.toml");
        match precondition {
            StatePrecondition::Absent => {
                if state_path.exists() {
                    return Err(TransactionError::Precondition(
                        "manager state exists but the plan requires it to be absent".to_string(),
                    ));
                }
            }
            StatePrecondition::Sha256 { value } => {
                ensure_regular_file(&state_path)?;
                let observed = sha256_file(&state_path)?;
                if observed != *value {
                    return Err(TransactionError::Precondition(format!(
                        "manager state changed after planning: expected {value}, observed {observed}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn apply_operation(
        &self,
        operation: &Operation,
        index: usize,
        transaction_id: &str,
        backup_dir: &Path,
        applied: &mut Vec<AppliedChange>,
    ) -> Result<ApplyRecord, TransactionError> {
        match operation {
            Operation::CreateDirectory { path, .. } => {
                let target = self.project_root.join(path);
                if target.is_dir() {
                    return Ok(ApplyRecord::default());
                }
                self.require_existing_safe_parent(&target)?;
                fs::create_dir(&target).map_err(|error| {
                    TransactionError::io("create managed directory", &target, error)
                })?;
                applied.push(AppliedChange::CreatedDirectory {
                    operation_index: index,
                    path: target,
                });
                Ok(ApplyRecord::default())
            }
            Operation::CreateFile { path, content, .. } => {
                let target = self.project_root.join(path);
                if target.exists() {
                    return Err(TransactionError::Precondition(format!(
                        "create target appeared after planning: {}",
                        target.display()
                    )));
                }
                self.require_existing_safe_parent(&target)?;
                let staged = write_staged_file(
                    &target,
                    content.as_bytes(),
                    transaction_id,
                    index,
                    None,
                )?;
                fs::rename(&staged, &target).map_err(|error| {
                    let _ = fs::remove_file(&staged);
                    TransactionError::io("install staged file", &target, error)
                })?;
                let after_sha256 = sha256_file(&target)?;
                applied.push(AppliedChange::CreatedFile {
                    operation_index: index,
                    path: target,
                    after_sha256: after_sha256.clone(),
                });
                Ok(ApplyRecord {
                    before_sha256: None,
                    after_sha256: Some(after_sha256),
                })
            }
            Operation::ReplaceOwnedFile {
                path,
                content,
                expected_sha256,
                ..
            } => {
                let target = self.project_root.join(path);
                ensure_regular_file(&target)?;
                let observed = sha256_file(&target)?;
                if observed != *expected_sha256 {
                    return Err(TransactionError::Precondition(format!(
                        "{} changed immediately before replacement",
                        path.display()
                    )));
                }
                let permissions = fs::metadata(&target)
                    .map_err(|error| TransactionError::io("read file metadata", &target, error))?
                    .permissions();
                let staged = write_staged_file(
                    &target,
                    content.as_bytes(),
                    transaction_id,
                    index,
                    Some(permissions),
                )?;
                let backup = backup_dir.join(format!("operation-{index:04}"));
                move_to_backup(&target, &backup)?;
                if let Err(error) = fs::rename(&staged, &target) {
                    let restore = fs::rename(&backup, &target);
                    let _ = fs::remove_file(&staged);
                    return match restore {
                        Ok(()) => Err(TransactionError::io(
                            "install staged replacement",
                            &target,
                            error,
                        )),
                        Err(restore_error) => Err(TransactionError::ExecutionFailed {
                            message: format!(
                                "replacement failed ({error}) and original restore failed ({restore_error})"
                            ),
                            journal_dir: backup_dir
                                .parent()
                                .unwrap_or(backup_dir)
                                .to_path_buf(),
                            recovery_required: true,
                        }),
                    };
                }
                let after_sha256 = sha256_file(&target)?;
                applied.push(AppliedChange::ReplacedFile {
                    operation_index: index,
                    path: target,
                    backup,
                    before_sha256: observed.clone(),
                    after_sha256: after_sha256.clone(),
                });
                Ok(ApplyRecord {
                    before_sha256: Some(observed),
                    after_sha256: Some(after_sha256),
                })
            }
            Operation::RemoveOwnedFile {
                path,
                expected_sha256,
                ..
            } => {
                let target = self.project_root.join(path);
                ensure_regular_file(&target)?;
                let observed = sha256_file(&target)?;
                if observed != *expected_sha256 {
                    return Err(TransactionError::Precondition(format!(
                        "{} changed immediately before removal",
                        path.display()
                    )));
                }
                let backup = backup_dir.join(format!("operation-{index:04}"));
                move_to_backup(&target, &backup)?;
                applied.push(AppliedChange::RemovedFile {
                    operation_index: index,
                    path: target,
                    backup,
                    before_sha256: observed.clone(),
                });
                Ok(ApplyRecord {
                    before_sha256: Some(observed),
                    after_sha256: None,
                })
            }
            Operation::MergeJson { .. }
            | Operation::MergeToml { .. }
            | Operation::NativePackageInstall { .. }
            | Operation::NativePackageUpdate { .. }
            | Operation::NativePackageRemove { .. } => Err(TransactionError::Unsupported(
                format!("{} cannot be executed in this slice", operation.kind()),
            )),
        }
    }

    fn require_existing_safe_parent(&self, target: &Path) -> Result<(), TransactionError> {
        let parent = target.parent().ok_or_else(|| {
            TransactionError::Precondition(format!(
                "target has no parent directory: {}",
                target.display()
            ))
        })?;
        ensure_safe_existing_path(parent)?;
        if !parent.is_dir() {
            return Err(TransactionError::Precondition(format!(
                "target parent is not a directory: {}",
                parent.display()
            )));
        }
        Ok(())
    }

    fn post_validate(&self, plan: &OperationPlan) -> Result<(), TransactionError> {
        for operation in plan.operations() {
            match operation {
                Operation::CreateDirectory { path, .. } => {
                    let target = self.project_root.join(path);
                    ensure_safe_existing_path(&target)?;
                    if !target.is_dir() {
                        return Err(TransactionError::Precondition(format!(
                            "created directory is absent: {}",
                            target.display()
                        )));
                    }
                }
                Operation::CreateFile { path, content, .. }
                | Operation::ReplaceOwnedFile { path, content, .. } => {
                    let target = self.project_root.join(path);
                    ensure_regular_file(&target)?;
                    let observed = sha256_file(&target)?;
                    let expected = sha256_bytes(content.as_bytes());
                    if observed != expected {
                        return Err(TransactionError::Precondition(format!(
                            "post-write digest mismatch for {}",
                            target.display()
                        )));
                    }
                }
                Operation::RemoveOwnedFile { path, .. } => {
                    let target = self.project_root.join(path);
                    if target.exists() {
                        return Err(TransactionError::Precondition(format!(
                            "removed file still exists: {}",
                            target.display()
                        )));
                    }
                }
                Operation::MergeJson { .. }
                | Operation::MergeToml { .. }
                | Operation::NativePackageInstall { .. }
                | Operation::NativePackageUpdate { .. }
                | Operation::NativePackageRemove { .. } => {
                    return Err(TransactionError::Unsupported(format!(
                        "{} cannot be post-validated in this slice",
                        operation.kind()
                    )));
                }
            }
        }
        Ok(())
    }

    fn write_state(
        &self,
        plan: &OperationPlan,
        backup_dir: &Path,
        applied: &mut Vec<AppliedChange>,
    ) -> Result<(), TransactionError> {
        let state = plan.desired_state().ok_or_else(|| {
            TransactionError::InvalidPlan("state write requested without desired state".to_string())
        })?;
        state.validate_for(plan.transaction_id())?;
        self.check_state_precondition(plan)?;
        let content = serialize_state(state)?;
        let target = self.project_root.join(".demoswarm/state.toml");
        let staged = write_staged_file(
            &target,
            content.as_bytes(),
            plan.transaction_id(),
            usize::MAX,
            None,
        )?;
        match plan.state_precondition() {
            Some(StatePrecondition::Absent) => {
                fs::rename(&staged, &target).map_err(|error| {
                    let _ = fs::remove_file(&staged);
                    TransactionError::io("install manager state", &target, error)
                })?;
                let after_sha256 = sha256_file(&target)?;
                applied.push(AppliedChange::CreatedState {
                    path: target,
                    after_sha256,
                });
            }
            Some(StatePrecondition::Sha256 { value }) => {
                let backup = backup_dir.join("state.toml");
                move_to_backup(&target, &backup)?;
                if let Err(error) = fs::rename(&staged, &target) {
                    let restore = fs::rename(&backup, &target);
                    let _ = fs::remove_file(&staged);
                    return match restore {
                        Ok(()) => Err(TransactionError::io(
                            "install manager state",
                            &target,
                            error,
                        )),
                        Err(restore_error) => Err(TransactionError::ExecutionFailed {
                            message: format!(
                                "state write failed ({error}) and prior state restore failed ({restore_error})"
                            ),
                            journal_dir: backup_dir
                                .parent()
                                .unwrap_or(backup_dir)
                                .to_path_buf(),
                            recovery_required: true,
                        }),
                    };
                }
                let after_sha256 = sha256_file(&target)?;
                applied.push(AppliedChange::ReplacedState {
                    path: target,
                    backup,
                    before_sha256: value.clone(),
                    after_sha256,
                });
            }
            None => {
                let _ = fs::remove_file(&staged);
                return Err(TransactionError::InvalidPlan(
                    "state write has no precondition".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn fail_transaction(
        &self,
        message: String,
        journal_dir: &Path,
        journal: &mut TransactionJournal,
        applied: &mut Vec<AppliedChange>,
        options: ExecutionOptions,
    ) -> TransactionError {
        journal.error = Some(message.clone());
        journal.phase = TransactionPhase::RollingBack;
        let mut journal_write_failed = journal
            .persist(journal_dir)
            .err()
            .map(|error| error.to_string());

        let rollback = self.rollback(
            applied,
            journal,
            journal_dir,
            options.rollback_failure_after,
        );
        match rollback {
            Ok(()) => {
                journal.phase = TransactionPhase::RolledBack;
                journal.recovery_required = journal_write_failed.is_some();
                if let Some(error) = &journal_write_failed {
                    journal.error = Some(format!(
                        "{message}; journal persistence also failed: {error}"
                    ));
                }
                if let Err(error) = journal.persist(journal_dir) {
                    journal_write_failed = Some(error.to_string());
                    journal.recovery_required = true;
                }
                let _ = TransactionReceipt::from_journal(journal).persist(journal_dir);
                TransactionError::ExecutionFailed {
                    message: journal_write_failed
                        .map(|error| format!("{message}; journal persistence failed: {error}"))
                        .unwrap_or(message),
                    journal_dir: journal_dir.to_path_buf(),
                    recovery_required: journal.recovery_required,
                }
            }
            Err(rollback_error) => {
                journal.phase = TransactionPhase::RecoveryRequired;
                journal.recovery_required = true;
                journal.error = Some(format!("{message}; rollback failed: {rollback_error}"));
                let _ = journal.persist(journal_dir);
                let _ = TransactionReceipt::from_journal(journal).persist(journal_dir);
                TransactionError::ExecutionFailed {
                    message: format!("{message}; rollback failed: {rollback_error}"),
                    journal_dir: journal_dir.to_path_buf(),
                    recovery_required: true,
                }
            }
        }
    }

    fn rollback(
        &self,
        applied: &mut Vec<AppliedChange>,
        journal: &mut TransactionJournal,
        journal_dir: &Path,
        rollback_failure_after: Option<usize>,
    ) -> Result<(), TransactionError> {
        let mut rolled_back = 0usize;
        while let Some(change) = applied.pop() {
            if rollback_failure_after == Some(rolled_back) {
                return Err(TransactionError::Precondition(format!(
                    "injected rollback failure after {rolled_back} restoration(s)"
                )));
            }
            let operation_index = change.operation_index();
            self.rollback_change(change)?;
            if let Some(index) = operation_index {
                let entry = journal.operation_mut(index)?;
                entry.status = JournalOperationStatus::RolledBack;
                entry.error = None;
            }
            journal.state_written = false;
            rolled_back += 1;
            journal.persist(journal_dir)?;
        }
        for entry in &mut journal.operations {
            if entry.status == JournalOperationStatus::Applied {
                entry.status = JournalOperationStatus::RolledBack;
            }
        }
        Ok(())
    }

    fn rollback_change(&self, change: AppliedChange) -> Result<(), TransactionError> {
        match change {
            AppliedChange::CreatedDirectory { path, .. } => fs::remove_dir(&path)
                .map_err(|error| TransactionError::io("remove created directory", &path, error)),
            AppliedChange::CreatedFile {
                path,
                after_sha256,
                ..
            }
            | AppliedChange::CreatedState {
                path,
                after_sha256,
            } => {
                ensure_digest(&path, &after_sha256)?;
                fs::remove_file(&path)
                    .map_err(|error| TransactionError::io("remove created file", &path, error))
            }
            AppliedChange::ReplacedFile {
                path,
                backup,
                before_sha256,
                after_sha256,
                ..
            }
            | AppliedChange::ReplacedState {
                path,
                backup,
                before_sha256,
                after_sha256,
            } => {
                ensure_digest(&path, &after_sha256)?;
                fs::remove_file(&path)
                    .map_err(|error| TransactionError::io("remove replacement", &path, error))?;
                fs::rename(&backup, &path).map_err(|error| {
                    TransactionError::io("restore original file", &path, error)
                })?;
                let restored = sha256_file(&path)?;
                if restored != before_sha256 {
                    return Err(TransactionError::Precondition(format!(
                        "restored file digest mismatch for {}",
                        path.display()
                    )));
                }
                Ok(())
            }
            AppliedChange::RemovedFile {
                path,
                backup,
                before_sha256,
                ..
            } => {
                if path.exists() {
                    return Err(TransactionError::Precondition(format!(
                        "cannot restore removed file because target reappeared: {}",
                        path.display()
                    )));
                }
                fs::rename(&backup, &path).map_err(|error| {
                    TransactionError::io("restore removed file", &path, error)
                })?;
                let restored = sha256_file(&path)?;
                if restored != before_sha256 {
                    return Err(TransactionError::Precondition(format!(
                        "restored removed-file digest mismatch for {}",
                        path.display()
                    )));
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Default)]
struct ApplyRecord {
    before_sha256: Option<String>,
    after_sha256: Option<String>,
}

#[derive(Debug)]
enum AppliedChange {
    CreatedDirectory {
        operation_index: usize,
        path: PathBuf,
    },
    CreatedFile {
        operation_index: usize,
        path: PathBuf,
        after_sha256: String,
    },
    ReplacedFile {
        operation_index: usize,
        path: PathBuf,
        backup: PathBuf,
        before_sha256: String,
        after_sha256: String,
    },
    RemovedFile {
        operation_index: usize,
        path: PathBuf,
        backup: PathBuf,
        before_sha256: String,
    },
    CreatedState {
        path: PathBuf,
        after_sha256: String,
    },
    ReplacedState {
        path: PathBuf,
        backup: PathBuf,
        before_sha256: String,
        after_sha256: String,
    },
}

impl AppliedChange {
    fn operation_index(&self) -> Option<usize> {
        match self {
            Self::CreatedDirectory {
                operation_index, ..
            }
            | Self::CreatedFile {
                operation_index, ..
            }
            | Self::ReplacedFile {
                operation_index, ..
            }
            | Self::RemovedFile {
                operation_index, ..
            } => Some(*operation_index),
            Self::CreatedState { .. } | Self::ReplacedState { .. } => None,
        }
    }
}

struct ProjectLock {
    file: File,
}

impl ProjectLock {
    fn acquire(manager_dir: &Path, transaction_id: &str) -> Result<Self, TransactionError> {
        let path = manager_dir.join("transaction.lock");
        if path.exists() {
            ensure_safe_existing_path(&path)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .map_err(|error| TransactionError::io("open transaction lock", &path, error))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let mut owner = String::new();
                let _ = file.read_to_string(&mut owner);
                return Err(TransactionError::Concurrent(format!(
                    "project lock is held{}",
                    if owner.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" by transaction `{}`", owner.trim())
                    }
                )));
            }
            Err(TryLockError::Error(error)) => {
                return Err(TransactionError::io(
                    "acquire transaction lock",
                    &path,
                    error,
                ));
            }
        }
        file.set_len(0)
            .map_err(|error| TransactionError::io("truncate transaction lock", &path, error))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| TransactionError::io("seek transaction lock", &path, error))?;
        file.write_all(transaction_id.as_bytes())
            .map_err(|error| TransactionError::io("write transaction lock", &path, error))?;
        file.sync_all()
            .map_err(|error| TransactionError::io("sync transaction lock", &path, error))?;
        Ok(Self { file })
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn backup_relative_path(
    transaction_id: &str,
    index: usize,
    operation: &Operation,
) -> Option<PathBuf> {
    if matches!(
        operation,
        Operation::ReplaceOwnedFile { .. } | Operation::RemoveOwnedFile { .. }
    ) {
        Some(
            PathBuf::from(".demoswarm")
                .join("operations")
                .join(transaction_id)
                .join("backups")
                .join(format!("operation-{index:04}")),
        )
    } else {
        None
    }
}

fn write_staged_file(
    target: &Path,
    bytes: &[u8],
    transaction_id: &str,
    index: usize,
    permissions: Option<fs::Permissions>,
) -> Result<PathBuf, TransactionError> {
    let parent = target.parent().ok_or_else(|| {
        TransactionError::Precondition(format!("target has no parent: {}", target.display()))
    })?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            TransactionError::Precondition(format!(
                "target filename is not UTF-8: {}",
                target.display()
            ))
        })?;
    let staged = parent.join(format!(
        ".{file_name}.demoswarm-{transaction_id}-{index}.tmp"
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .map_err(|error| TransactionError::io("create staged file", &staged, error))?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)
            .map_err(|error| TransactionError::io("set staged permissions", &staged, error))?;
    }
    file.write_all(bytes)
        .map_err(|error| TransactionError::io("write staged file", &staged, error))?;
    file.sync_all()
        .map_err(|error| TransactionError::io("sync staged file", &staged, error))?;
    Ok(staged)
}

fn move_to_backup(target: &Path, backup: &Path) -> Result<(), TransactionError> {
    if backup.exists() {
        return Err(TransactionError::Precondition(format!(
            "backup path already exists: {}",
            backup.display()
        )));
    }
    fs::rename(target, backup)
        .map_err(|error| TransactionError::io("move original to backup", target, error))
}

fn serialize_state(state: &ManagerState) -> Result<String, TransactionError> {
    let mut value = toml::to_string_pretty(state)
        .map_err(|error| TransactionError::Serialization(error.to_string()))?;
    if !value.ends_with('\n') {
        value.push('\n');
    }
    Ok(value)
}

fn ensure_regular_file(path: &Path) -> Result<(), TransactionError> {
    ensure_safe_existing_path(path)?;
    if !path.is_file() {
        return Err(TransactionError::Precondition(format!(
            "expected regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_safe_existing_path(path: &Path) -> Result<(), TransactionError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| TransactionError::io("inspect path", path, error))?;
    if is_link_like(&metadata) {
        return Err(TransactionError::Precondition(format!(
            "refusing to traverse or mutate link/reparse path: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn is_link_like(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link_like(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn ensure_digest(path: &Path, expected: &str) -> Result<(), TransactionError> {
    if !path.exists() {
        return Err(TransactionError::Precondition(format!(
            "rollback target is absent: {}",
            path.display()
        )));
    }
    ensure_regular_file(path)?;
    let observed = sha256_file(path)?;
    if observed != expected {
        return Err(TransactionError::Precondition(format!(
            "rollback target changed after application: {}",
            path.display()
        )));
    }
    Ok(())
}
