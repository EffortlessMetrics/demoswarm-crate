use serde::Serialize;
use serde_json::Value;

pub const EXIT_SUCCESS: u8 = 0;
pub const EXIT_OPERATIONAL: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_ENVIRONMENT: u8 = 3;
pub const EXIT_UNSUPPORTED: u8 = 4;
pub const EXIT_INVALID_STATE: u8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Notice,
    Pass,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub subject: String,
    pub message: String,
    pub fixable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Diagnostic {
    pub fn new(
        code: impl Into<String>,
        severity: Severity,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            subject: subject.into(),
            message: message.into(),
            fixable: false,
            remediation: None,
        }
    }

    pub fn with_remediation(mut self, remediation: impl Into<String>, fixable: bool) -> Self {
        self.fixable = fixable;
        self.remediation = Some(remediation.into());
        self
    }
}

#[derive(Debug, Serialize)]
pub struct Envelope {
    pub schema_version: u32,
    pub command: String,
    pub ok: bool,
    pub dry_run: bool,
    pub project: Option<String>,
    pub data: Value,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug)]
pub struct CommandResult {
    pub envelope: Envelope,
    pub human_lines: Vec<String>,
    pub exit_code: u8,
}

impl CommandResult {
    pub fn success(
        command: impl Into<String>,
        dry_run: bool,
        project: Option<String>,
        data: Value,
        human_lines: Vec<String>,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            envelope: Envelope {
                schema_version: 1,
                command: command.into(),
                ok: true,
                dry_run,
                project,
                data,
                diagnostics,
            },
            human_lines,
            exit_code: EXIT_SUCCESS,
        }
    }

    pub fn failure(
        command: impl Into<String>,
        dry_run: bool,
        project: Option<String>,
        data: Value,
        human_lines: Vec<String>,
        diagnostics: Vec<Diagnostic>,
        exit_code: u8,
    ) -> Self {
        Self {
            envelope: Envelope {
                schema_version: 1,
                command: command.into(),
                ok: false,
                dry_run,
                project,
                data,
                diagnostics,
            },
            human_lines,
            exit_code,
        }
    }
}
