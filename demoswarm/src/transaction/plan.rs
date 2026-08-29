use super::{ManagerState, StatePrecondition, TransactionError, is_sha256, sha256_bytes};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use toml::Value as TomlValue;

/// How the manager relates to an installed path or semantic value.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipStrategy {
    Owned,
    Merged,
    Shared,
    UserOwned,
    Unmanaged,
}

/// Ownership evidence attached to a typed mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ownership {
    pub strategy: OwnershipStrategy,
    pub owners: BTreeSet<String>,
}

impl Ownership {
    #[must_use]
    pub fn owned(owner: impl Into<String>) -> Self {
        Self {
            strategy: OwnershipStrategy::Owned,
            owners: BTreeSet::from([owner.into()]),
        }
    }

    pub fn shared<I, S>(owners: I) -> Result<Self, TransactionError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let ownership = Self {
            strategy: OwnershipStrategy::Shared,
            owners: owners.into_iter().map(Into::into).collect(),
        };
        ownership.validate_mutating(false)?;
        Ok(ownership)
    }

    pub fn merged<I, S>(owners: I) -> Result<Self, TransactionError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let ownership = Self {
            strategy: OwnershipStrategy::Merged,
            owners: owners.into_iter().map(Into::into).collect(),
        };
        ownership.validate_mutating(true)?;
        Ok(ownership)
    }

    pub(crate) fn validate_mutating(&self, semantic: bool) -> Result<(), TransactionError> {
        if self.owners.iter().any(|owner| owner.trim().is_empty()) {
            return Err(TransactionError::InvalidPlan(
                "ownership contains an empty owner ID".to_string(),
            ));
        }
        match self.strategy {
            OwnershipStrategy::Owned => {
                if semantic || self.owners.len() != 1 {
                    return Err(TransactionError::InvalidPlan(
                        "owned filesystem mutations require exactly one owner".to_string(),
                    ));
                }
            }
            OwnershipStrategy::Shared => {
                if self.owners.is_empty() {
                    return Err(TransactionError::InvalidPlan(
                        "shared filesystem mutations require at least one owner".to_string(),
                    ));
                }
            }
            OwnershipStrategy::Merged => {
                if !semantic || self.owners.is_empty() {
                    return Err(TransactionError::InvalidPlan(
                        "merged ownership is valid only for semantic mutations with an owner"
                            .to_string(),
                    ));
                }
            }
            OwnershipStrategy::UserOwned | OwnershipStrategy::Unmanaged => {
                return Err(TransactionError::InvalidPlan(format!(
                    "a mutating operation cannot claim {:?} ownership",
                    self.strategy
                )));
            }
        }
        Ok(())
    }
}

/// A known command that can compensate a native package operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Compensation {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Lifecycle action represented by a native package operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativePackageAction {
    Install,
    Update,
    Remove,
}

/// The complete typed operation vocabulary accepted by the transaction planner.
///
/// Native package and semantic merge variants are intentionally modeled now so
/// adapters cannot invent ad-hoc shell execution. The first executor slice only
/// applies full-file operations; later issues add reviewed executors for the other
/// typed variants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Operation {
    CreateDirectory {
        path: PathBuf,
        ownership: Ownership,
    },
    CreateFile {
        path: PathBuf,
        content: String,
        ownership: Ownership,
    },
    ReplaceOwnedFile {
        path: PathBuf,
        content: String,
        expected_sha256: String,
        ownership: Ownership,
    },
    RemoveOwnedFile {
        path: PathBuf,
        expected_sha256: String,
        ownership: Ownership,
    },
    MergeJson {
        path: PathBuf,
        pointer: String,
        value: JsonValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_value_sha256: Option<String>,
        ownership: Ownership,
    },
    MergeToml {
        path: PathBuf,
        dotted_key: String,
        value: TomlValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_value_sha256: Option<String>,
        ownership: Ownership,
    },
    NativePackageInstall {
        adapter: String,
        identity: String,
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compensation: Option<Compensation>,
    },
    NativePackageUpdate {
        adapter: String,
        identity: String,
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compensation: Option<Compensation>,
    },
    NativePackageRemove {
        adapter: String,
        identity: String,
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compensation: Option<Compensation>,
    },
}

impl Operation {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CreateDirectory { .. } => "create_directory",
            Self::CreateFile { .. } => "create_file",
            Self::ReplaceOwnedFile { .. } => "replace_owned_file",
            Self::RemoveOwnedFile { .. } => "remove_owned_file",
            Self::MergeJson { .. } => "merge_json",
            Self::MergeToml { .. } => "merge_toml",
            Self::NativePackageInstall { .. } => "native_package_install",
            Self::NativePackageUpdate { .. } => "native_package_update",
            Self::NativePackageRemove { .. } => "native_package_remove",
        }
    }

    #[must_use]
    pub fn target_path(&self) -> Option<&Path> {
        match self {
            Self::CreateDirectory { path, .. }
            | Self::CreateFile { path, .. }
            | Self::ReplaceOwnedFile { path, .. }
            | Self::RemoveOwnedFile { path, .. }
            | Self::MergeJson { path, .. }
            | Self::MergeToml { path, .. } => Some(path),
            Self::NativePackageInstall { .. }
            | Self::NativePackageUpdate { .. }
            | Self::NativePackageRemove { .. } => None,
        }
    }

    #[must_use]
    pub fn native_action(&self) -> Option<NativePackageAction> {
        match self {
            Self::NativePackageInstall { .. } => Some(NativePackageAction::Install),
            Self::NativePackageUpdate { .. } => Some(NativePackageAction::Update),
            Self::NativePackageRemove { .. } => Some(NativePackageAction::Remove),
            _ => None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), TransactionError> {
        if let Some(path) = self.target_path() {
            validate_relative_path(path)?;
            validate_not_reserved(path)?;
        }

        match self {
            Self::CreateDirectory { ownership, .. } | Self::CreateFile { ownership, .. } => {
                ownership.validate_mutating(false)
            }
            Self::ReplaceOwnedFile {
                expected_sha256,
                ownership,
                ..
            }
            | Self::RemoveOwnedFile {
                expected_sha256,
                ownership,
                ..
            } => {
                if !is_sha256(expected_sha256) {
                    return Err(TransactionError::InvalidPlan(format!(
                        "{} requires a lowercase SHA-256 precondition",
                        self.kind()
                    )));
                }
                ownership.validate_mutating(false)
            }
            Self::MergeJson {
                pointer,
                expected_value_sha256,
                ownership,
                ..
            } => {
                if !pointer.starts_with('/') || pointer.len() == 1 {
                    return Err(TransactionError::InvalidPlan(
                        "JSON merge pointer must be a non-root RFC 6901 pointer".to_string(),
                    ));
                }
                validate_optional_digest(expected_value_sha256)?;
                ownership.validate_mutating(true)
            }
            Self::MergeToml {
                dotted_key,
                expected_value_sha256,
                ownership,
                ..
            } => {
                if dotted_key
                    .split('.')
                    .any(|component| component.trim().is_empty())
                {
                    return Err(TransactionError::InvalidPlan(
                        "TOML merge key must contain non-empty dotted components".to_string(),
                    ));
                }
                validate_optional_digest(expected_value_sha256)?;
                ownership.validate_mutating(true)
            }
            Self::NativePackageInstall {
                adapter,
                identity,
                program,
                args,
                compensation,
            }
            | Self::NativePackageUpdate {
                adapter,
                identity,
                program,
                args,
                compensation,
            }
            | Self::NativePackageRemove {
                adapter,
                identity,
                program,
                args,
                compensation,
            } => validate_native_command(adapter, identity, program, args, compensation.as_ref()),
        }
    }

    fn conflict_key(&self) -> Option<(PathBuf, String)> {
        match self {
            Self::MergeJson { path, pointer, .. } => {
                Some((path.clone(), format!("json:{pointer}")))
            }
            Self::MergeToml {
                path, dotted_key, ..
            } => Some((path.clone(), format!("toml:{dotted_key}"))),
            _ => self
                .target_path()
                .map(|path| (path.to_path_buf(), "whole".to_string())),
        }
    }

    fn native_key(&self) -> Option<(String, String)> {
        match self {
            Self::NativePackageInstall {
                adapter, identity, ..
            }
            | Self::NativePackageUpdate {
                adapter, identity, ..
            }
            | Self::NativePackageRemove {
                adapter, identity, ..
            } => Some((adapter.clone(), identity.clone())),
            _ => None,
        }
    }
}

/// A serializable, authenticated change plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperationPlan {
    schema_version: u32,
    transaction_id: String,
    command: String,
    project_root: PathBuf,
    operations: Vec<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desired_state: Option<ManagerState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state_precondition: Option<StatePrecondition>,
    plan_digest: String,
}

impl OperationPlan {
    pub fn new(
        transaction_id: impl Into<String>,
        command: impl Into<String>,
        project_root: impl Into<PathBuf>,
        operations: Vec<Operation>,
    ) -> Result<Self, TransactionError> {
        let mut plan = Self {
            schema_version: 1,
            transaction_id: transaction_id.into(),
            command: command.into(),
            project_root: project_root.into(),
            operations,
            desired_state: None,
            state_precondition: None,
            plan_digest: String::new(),
        };
        plan.reseal()?;
        Ok(plan)
    }

    pub fn with_state(
        mut self,
        desired_state: ManagerState,
        precondition: StatePrecondition,
    ) -> Result<Self, TransactionError> {
        self.desired_state = Some(desired_state);
        self.state_precondition = Some(precondition);
        self.reseal()?;
        Ok(self)
    }

    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub fn transaction_id(&self) -> &str {
        &self.transaction_id
    }

    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    #[must_use]
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    #[must_use]
    pub fn desired_state(&self) -> Option<&ManagerState> {
        self.desired_state.as_ref()
    }

    #[must_use]
    pub fn state_precondition(&self) -> Option<&StatePrecondition> {
        self.state_precondition.as_ref()
    }

    #[must_use]
    pub fn plan_digest(&self) -> &str {
        &self.plan_digest
    }

    pub fn validate(&self) -> Result<(), TransactionError> {
        self.validate_structure()?;
        let expected = self.compute_digest()?;
        if self.plan_digest != expected {
            return Err(TransactionError::InvalidPlan(format!(
                "plan digest mismatch: expected {expected}, observed {}",
                self.plan_digest
            )));
        }
        Ok(())
    }

    fn reseal(&mut self) -> Result<(), TransactionError> {
        self.plan_digest.clear();
        self.validate_structure()?;
        self.plan_digest = self.compute_digest()?;
        Ok(())
    }

    fn validate_structure(&self) -> Result<(), TransactionError> {
        if self.schema_version != 1 {
            return Err(TransactionError::InvalidPlan(format!(
                "unsupported plan schema version {}",
                self.schema_version
            )));
        }
        validate_transaction_id(&self.transaction_id)?;
        if self.command.trim().is_empty() {
            return Err(TransactionError::InvalidPlan(
                "transaction command is empty".to_string(),
            ));
        }
        if !self.project_root.is_absolute() {
            return Err(TransactionError::InvalidPlan(
                "project root must be absolute".to_string(),
            ));
        }
        if self.operations.is_empty() && self.desired_state.is_none() {
            return Err(TransactionError::InvalidPlan(
                "transaction plan has no operations or state change".to_string(),
            ));
        }
        match (&self.desired_state, &self.state_precondition) {
            (Some(state), Some(precondition)) => {
                state.validate_for(&self.transaction_id)?;
                precondition.validate()?;
            }
            (None, None) => {}
            _ => {
                return Err(TransactionError::InvalidPlan(
                    "desired state and state precondition must be supplied together".to_string(),
                ));
            }
        }

        let mut path_uses: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
        let mut native_uses = BTreeSet::new();
        for operation in &self.operations {
            operation.validate()?;
            if let Some((path, use_kind)) = operation.conflict_key() {
                let existing = path_uses.entry(path.clone()).or_default();
                if existing.contains("whole") || use_kind == "whole" && !existing.is_empty() {
                    return Err(TransactionError::InvalidPlan(format!(
                        "conflicting operations target {}",
                        path.display()
                    )));
                }
                if !existing.insert(use_kind) {
                    return Err(TransactionError::InvalidPlan(format!(
                        "duplicate operation target {}",
                        path.display()
                    )));
                }
            }
            if let Some(key) = operation.native_key()
                && !native_uses.insert(key.clone())
            {
                return Err(TransactionError::InvalidPlan(format!(
                    "duplicate native package operation for {}/{}",
                    key.0, key.1
                )));
            }
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<String, TransactionError> {
        let mut digest_view = self.clone();
        digest_view.plan_digest.clear();
        serde_json::to_vec(&digest_view)
            .map(|bytes| sha256_bytes(&bytes))
            .map_err(|error| TransactionError::Serialization(error.to_string()))
    }
}

fn validate_transaction_id(value: &str) -> Result<(), TransactionError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        return Err(TransactionError::InvalidPlan(
            "transaction ID must be 1-128 ASCII letters, digits, dots, dashes, or underscores"
                .to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_relative_path(path: &Path) -> Result<(), TransactionError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(TransactionError::InvalidPlan(format!(
            "operation path must be non-empty and project-relative: {}",
            path.display()
        )));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(TransactionError::InvalidPlan(format!(
                "operation path contains a non-normal component: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_not_reserved(path: &Path) -> Result<(), TransactionError> {
    let components: Vec<_> = path.components().collect();
    let first = components
        .first()
        .and_then(|component| component.as_os_str().to_str());
    let second = components
        .get(1)
        .and_then(|component| component.as_os_str().to_str());
    if first == Some(".demoswarm")
        && matches!(
            second,
            Some("operations" | "state.toml" | "transaction.lock")
        )
    {
        return Err(TransactionError::InvalidPlan(format!(
            "operation targets reserved transaction state: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_optional_digest(value: &Option<String>) -> Result<(), TransactionError> {
    if let Some(value) = value
        && !is_sha256(value)
    {
        return Err(TransactionError::InvalidPlan(
            "semantic merge precondition must be lowercase SHA-256".to_string(),
        ));
    }
    Ok(())
}

fn validate_native_command(
    adapter: &str,
    identity: &str,
    program: &str,
    args: &[String],
    compensation: Option<&Compensation>,
) -> Result<(), TransactionError> {
    if adapter.trim().is_empty() || identity.trim().is_empty() || program.trim().is_empty() {
        return Err(TransactionError::InvalidPlan(
            "native package operation requires adapter, identity, and program".to_string(),
        ));
    }
    if program.contains('\0') || args.iter().any(|argument| argument.contains('\0')) {
        return Err(TransactionError::InvalidPlan(
            "native package command contains a NUL byte".to_string(),
        ));
    }
    if let Some(compensation) = compensation
        && (compensation.program.trim().is_empty()
            || compensation.program.contains('\0')
            || compensation
                .args
                .iter()
                .any(|argument| argument.contains('\0')))
    {
        return Err(TransactionError::InvalidPlan(
            "native package compensation is invalid".to_string(),
        ));
    }
    Ok(())
}
