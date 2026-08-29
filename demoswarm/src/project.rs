use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub enum DiscoveryKind {
    Explicit,
    GitRoot,
    DemoSwarmRoot,
    CurrentDirectory,
}

impl fmt::Display for DiscoveryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Explicit => "explicit",
            Self::GitRoot => "git-root",
            Self::DemoSwarmRoot => "demoswarm-root",
            Self::CurrentDirectory => "current-directory",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone)]
pub struct ProjectContext {
    root: PathBuf,
    discovery: DiscoveryKind,
}

impl ProjectContext {
    pub fn discover(explicit: Option<&Path>) -> Result<Self, ProjectError> {
        if let Some(path) = explicit {
            if !path.exists() {
                return Err(ProjectError::Missing(path.to_path_buf()));
            }
            if !path.is_dir() {
                return Err(ProjectError::NotDirectory(path.to_path_buf()));
            }
            let root = path
                .canonicalize()
                .map_err(|source| ProjectError::Io(path.to_path_buf(), source))?;
            return Ok(Self {
                root,
                discovery: DiscoveryKind::Explicit,
            });
        }

        let current = std::env::current_dir()
            .map_err(|source| ProjectError::Io(PathBuf::from("."), source))?;
        let current = current
            .canonicalize()
            .map_err(|source| ProjectError::Io(current.clone(), source))?;

        for candidate in current.ancestors() {
            if candidate.join(".demoswarm").is_dir() || candidate.join(".runs").is_dir() {
                return Ok(Self {
                    root: candidate.to_path_buf(),
                    discovery: DiscoveryKind::DemoSwarmRoot,
                });
            }
            if candidate.join(".git").exists() {
                return Ok(Self {
                    root: candidate.to_path_buf(),
                    discovery: DiscoveryKind::GitRoot,
                });
            }
        }

        Ok(Self {
            root: current,
            discovery: DiscoveryKind::CurrentDirectory,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn display_root(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    pub fn discovery(&self) -> DiscoveryKind {
        self.discovery
    }
}

#[derive(Debug)]
pub enum ProjectError {
    Missing(PathBuf),
    NotDirectory(PathBuf),
    Io(PathBuf, std::io::Error),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(formatter, "project path does not exist: {}", path.display()),
            Self::NotDirectory(path) => {
                write!(formatter, "project path is not a directory: {}", path.display())
            }
            Self::Io(path, source) => {
                write!(formatter, "could not inspect project path {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ProjectError {}
