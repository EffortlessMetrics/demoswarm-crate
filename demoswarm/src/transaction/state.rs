use super::{OwnershipStrategy, TransactionError, is_sha256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Preconditions for replacing the manager-owned local state document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StatePrecondition {
    /// `.demoswarm/state.toml` must not exist.
    Absent,
    /// The current state file must have the given SHA-256 digest.
    Sha256 { value: String },
}

impl StatePrecondition {
    pub(crate) fn validate(&self) -> Result<(), TransactionError> {
        if let Self::Sha256 { value } = self
            && !is_sha256(value)
        {
            return Err(TransactionError::InvalidPlan(
                "state precondition digest must be lowercase SHA-256".to_string(),
            ));
        }
        Ok(())
    }
}

/// Authenticated pack identity recorded after a successful lifecycle operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledPack {
    pub id: String,
    pub version: String,
    pub source: String,
    pub digest: String,
    pub verification: String,
}

/// Observed native host adapter state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledAdapter {
    pub id: String,
    pub version: String,
    pub scope: String,
    pub install_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_version: Option<String>,
}

/// Manager ownership evidence for a complete path or narrow semantic selector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedPathState {
    pub path: PathBuf,
    pub strategy: OwnershipStrategy,
    pub owners: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_value_sha256: Option<String>,
}

/// Identity of the last lifecycle operation committed to local manager state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastTransaction {
    pub id: String,
    pub command: String,
    pub status: String,
}

/// Manager-owned observed installation state.
///
/// This is deliberately distinct from `.demoswarm/config.toml` (project intent)
/// and `.runs/` (flow evidence).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagerState {
    pub schema_version: u32,
    pub generation: u64,
    pub manager_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<InstalledPack>,
    #[serde(default)]
    pub adapters: Vec<InstalledAdapter>,
    #[serde(default)]
    pub managed_paths: Vec<ManagedPathState>,
    pub last_transaction: LastTransaction,
}

impl ManagerState {
    pub fn validate_for(&self, transaction_id: &str) -> Result<(), TransactionError> {
        if self.schema_version != 1 {
            return Err(TransactionError::InvalidPlan(format!(
                "unsupported manager state schema version {}",
                self.schema_version
            )));
        }
        if self.manager_version.trim().is_empty() {
            return Err(TransactionError::InvalidPlan(
                "manager state must name the manager version".to_string(),
            ));
        }
        if self.last_transaction.id != transaction_id {
            return Err(TransactionError::InvalidPlan(format!(
                "desired state last_transaction `{}` does not match plan transaction `{transaction_id}`",
                self.last_transaction.id
            )));
        }
        if self.last_transaction.command.trim().is_empty() {
            return Err(TransactionError::InvalidPlan(
                "desired state last_transaction command is empty".to_string(),
            ));
        }

        if let Some(pack) = &self.pack {
            if pack.id.trim().is_empty()
                || pack.version.trim().is_empty()
                || pack.source.trim().is_empty()
                || pack.verification.trim().is_empty()
            {
                return Err(TransactionError::InvalidPlan(
                    "installed pack identity contains an empty required field".to_string(),
                ));
            }
            if !is_sha256(&pack.digest) {
                return Err(TransactionError::InvalidPlan(
                    "installed pack digest must be lowercase SHA-256".to_string(),
                ));
            }
        }

        let mut adapter_ids = BTreeSet::new();
        for adapter in &self.adapters {
            if adapter.id.trim().is_empty()
                || adapter.version.trim().is_empty()
                || adapter.scope.trim().is_empty()
                || adapter.install_mode.trim().is_empty()
            {
                return Err(TransactionError::InvalidPlan(
                    "installed adapter contains an empty required field".to_string(),
                ));
            }
            if !adapter_ids.insert(adapter.id.as_str()) {
                return Err(TransactionError::InvalidPlan(format!(
                    "duplicate installed adapter `{}`",
                    adapter.id
                )));
            }
        }

        let mut path_selectors = BTreeSet::new();
        for managed in &self.managed_paths {
            super::plan::validate_relative_path(&managed.path)?;
            if matches!(
                managed.strategy,
                OwnershipStrategy::UserOwned | OwnershipStrategy::Unmanaged
            ) {
                return Err(TransactionError::InvalidPlan(format!(
                    "manager state cannot claim {:?} path {}",
                    managed.strategy,
                    managed.path.display()
                )));
            }
            if managed.owners.is_empty() {
                return Err(TransactionError::InvalidPlan(format!(
                    "managed path {} has no owners",
                    managed.path.display()
                )));
            }
            if let Some(digest) = &managed.installed_sha256
                && !is_sha256(digest)
            {
                return Err(TransactionError::InvalidPlan(format!(
                    "managed path {} has an invalid file digest",
                    managed.path.display()
                )));
            }
            if let Some(digest) = &managed.installed_value_sha256
                && !is_sha256(digest)
            {
                return Err(TransactionError::InvalidPlan(format!(
                    "managed path {} has an invalid semantic-value digest",
                    managed.path.display()
                )));
            }
            let key = (
                managed.path.clone(),
                managed.selector.clone().unwrap_or_default(),
            );
            if !path_selectors.insert(key) {
                return Err(TransactionError::InvalidPlan(format!(
                    "duplicate managed path/selector state for {}",
                    managed.path.display()
                )));
            }
        }
        Ok(())
    }
}
