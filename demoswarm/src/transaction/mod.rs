//! Typed, recoverable lifecycle transactions.
//!
//! Host adapters describe desired changes as [`OperationPlan`] values. The shared
//! executor validates ownership and preconditions, journals each step, applies
//! manager-owned filesystem mutations, and rolls them back when validation fails.

mod executor;
mod journal;
mod plan;
mod state;

pub use executor::{
    ExecutionOptions, ExecutionOutcome, ExecutionReport, FailurePoint, TransactionExecutor,
};
pub use journal::{
    JournalOperation, JournalOperationStatus, TransactionJournal, TransactionPhase,
    TransactionReceipt, read_latest_journal,
};
pub use plan::{
    Compensation, NativePackageAction, Operation, OperationPlan, Ownership, OwnershipStrategy,
};
pub use state::{
    InstalledAdapter, InstalledPack, LastTransaction, ManagedPathState, ManagerState,
    StatePrecondition,
};

use sha2::{Digest, Sha256};
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Errors produced while validating or executing a lifecycle transaction.
#[derive(Debug)]
pub enum TransactionError {
    /// The serialized plan violates a structural invariant.
    InvalidPlan(String),
    /// The observed project state no longer matches the plan's preconditions.
    Precondition(String),
    /// Another manager operation currently owns the project lock.
    Concurrent(String),
    /// The operation is typed but intentionally not executable by this layer yet.
    Unsupported(String),
    /// Serialization of a plan, state document, journal, or receipt failed.
    Serialization(String),
    /// A filesystem operation failed.
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// Application failed after journaling began.
    ExecutionFailed {
        message: String,
        journal_dir: PathBuf,
        recovery_required: bool,
    },
}

impl TransactionError {
    pub(crate) fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for TransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlan(message) => write!(formatter, "invalid transaction plan: {message}"),
            Self::Precondition(message) => {
                write!(formatter, "transaction precondition failed: {message}")
            }
            Self::Concurrent(message) => write!(formatter, "concurrent transaction: {message}"),
            Self::Unsupported(message) => {
                write!(formatter, "unsupported transaction operation: {message}")
            }
            Self::Serialization(message) => {
                write!(formatter, "transaction serialization failed: {message}")
            }
            Self::Io {
                action,
                path,
                source,
            } => write!(formatter, "could not {action} {}: {source}", path.display()),
            Self::ExecutionFailed {
                message,
                journal_dir,
                recovery_required,
            } => write!(
                formatter,
                "transaction failed: {message}; journal: {}; recovery required: {recovery_required}",
                journal_dir.display()
            ),
        }
    }
}

impl Error for TransactionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Compute a lowercase SHA-256 digest for in-memory bytes.
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Compute a lowercase SHA-256 digest for a regular file.
pub fn sha256_file(path: &Path) -> Result<String, TransactionError> {
    let mut file = File::open(path).map_err(|error| TransactionError::io("open", path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| TransactionError::io("read", path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
