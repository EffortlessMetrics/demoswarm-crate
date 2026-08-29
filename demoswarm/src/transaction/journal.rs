use super::{OperationPlan, TransactionError};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Durable phase of a lifecycle transaction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransactionPhase {
    Planned,
    Applying,
    Validating,
    WritingState,
    RollingBack,
    RolledBack,
    RecoveryRequired,
    Committed,
}

/// Durable state of one typed operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JournalOperationStatus {
    Pending,
    Applied,
    RolledBack,
    RecoveryRequired,
}

/// Journal evidence for one typed operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalOperation {
    pub index: usize,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
    pub status: JournalOperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Append-only transaction journal snapshot.
///
/// Each persistence creates a new numbered file instead of replacing the prior
/// snapshot. A crash can therefore leave a stale snapshot, but cannot destroy the
/// last valid recovery record through a partially written replacement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionJournal {
    pub schema_version: u32,
    pub transaction_id: String,
    pub command: String,
    pub plan_digest: String,
    pub sequence: u64,
    pub phase: TransactionPhase,
    pub operations: Vec<JournalOperation>,
    pub state_written: bool,
    pub recovery_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TransactionJournal {
    #[must_use]
    pub fn new(plan: &OperationPlan) -> Self {
        let operations = plan
            .operations()
            .iter()
            .enumerate()
            .map(|(index, operation)| JournalOperation {
                index,
                kind: operation.kind().to_string(),
                target: operation.target_path().map(Path::to_path_buf),
                status: JournalOperationStatus::Pending,
                before_sha256: None,
                after_sha256: None,
                backup_path: None,
                error: None,
            })
            .collect();
        Self {
            schema_version: 1,
            transaction_id: plan.transaction_id().to_string(),
            command: plan.command().to_string(),
            plan_digest: plan.plan_digest().to_string(),
            sequence: 0,
            phase: TransactionPhase::Planned,
            operations,
            state_written: false,
            recovery_required: false,
            error: None,
        }
    }

    pub(crate) fn operation_mut(
        &mut self,
        index: usize,
    ) -> Result<&mut JournalOperation, TransactionError> {
        self.operations.get_mut(index).ok_or_else(|| {
            TransactionError::InvalidPlan(format!("journal has no operation at index {index}"))
        })
    }

    pub(crate) fn persist(&mut self, directory: &Path) -> Result<PathBuf, TransactionError> {
        fs::create_dir_all(directory)
            .map_err(|error| TransactionError::io("create journal directory", directory, error))?;
        self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
            TransactionError::Serialization("journal sequence overflow".to_string())
        })?;
        let path = directory.join(format!("journal-{:06}.json", self.sequence));
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| TransactionError::Serialization(error.to_string()))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| TransactionError::io("create journal snapshot", &path, error))?;
        file.write_all(&bytes)
            .map_err(|error| TransactionError::io("write journal snapshot", &path, error))?;
        file.write_all(b"\n")
            .map_err(|error| TransactionError::io("terminate journal snapshot", &path, error))?;
        file.sync_all()
            .map_err(|error| TransactionError::io("sync journal snapshot", &path, error))?;
        Ok(path)
    }
}

/// Immutable receipt written after a transaction reaches a terminal phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionReceipt {
    pub schema_version: u32,
    pub transaction_id: String,
    pub command: String,
    pub plan_digest: String,
    pub outcome: String,
    pub operations_applied: usize,
    pub state_written: bool,
    pub recovery_required: bool,
    pub final_journal_sequence: u64,
}

impl TransactionReceipt {
    pub(crate) fn from_journal(journal: &TransactionJournal) -> Self {
        let operations_applied = journal
            .operations
            .iter()
            .filter(|operation| operation.status == JournalOperationStatus::Applied)
            .count();
        Self {
            schema_version: 1,
            transaction_id: journal.transaction_id.clone(),
            command: journal.command.clone(),
            plan_digest: journal.plan_digest.clone(),
            outcome: match journal.phase {
                TransactionPhase::Committed => "committed",
                TransactionPhase::RolledBack => "rolled_back",
                TransactionPhase::RecoveryRequired => "recovery_required",
                _ => "nonterminal",
            }
            .to_string(),
            operations_applied,
            state_written: journal.state_written,
            recovery_required: journal.recovery_required,
            final_journal_sequence: journal.sequence,
        }
    }

    pub(crate) fn persist(&self, directory: &Path) -> Result<PathBuf, TransactionError> {
        let path = directory.join("receipt.json");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| TransactionError::Serialization(error.to_string()))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| TransactionError::io("create transaction receipt", &path, error))?;
        file.write_all(&bytes)
            .map_err(|error| TransactionError::io("write transaction receipt", &path, error))?;
        file.write_all(b"\n")
            .map_err(|error| TransactionError::io("terminate transaction receipt", &path, error))?;
        file.sync_all()
            .map_err(|error| TransactionError::io("sync transaction receipt", &path, error))?;
        Ok(path)
    }
}

/// Read the highest-numbered valid journal snapshot in a transaction directory.
pub fn read_latest_journal(directory: &Path) -> Result<TransactionJournal, TransactionError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| TransactionError::io("read journal directory", directory, error))?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| TransactionError::io("read journal entry", directory, error))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("journal-") && name.ends_with(".json") {
            candidates.push(entry.path());
        }
    }
    candidates.sort();
    let path = candidates.pop().ok_or_else(|| {
        TransactionError::Precondition(format!(
            "no journal snapshots exist in {}",
            directory.display()
        ))
    })?;
    let bytes = fs::read(&path)
        .map_err(|error| TransactionError::io("read journal snapshot", &path, error))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| TransactionError::Serialization(error.to_string()))
}
