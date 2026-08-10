use std::{fs, path::PathBuf, sync::LazyLock};

use anyhow::{Context, Result};
use regex::Regex;

use crate::{
    model::{PolicySeverity, PolicyViolation, PolicyViolationType, ScriptType, ScriptUsage},
    workflow::{discover_workflow_files, scan_workflow, scan_workflow_scripts},
};

static PERMISSIONS_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<indent>\s*)permissions:\s*(?P<value>[^#]*)").expect("valid permissions regex")
});

static PERMISSION_ENTRY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?P<scope>[A-Za-z0-9_-]+):\s*(?P<value>[A-Za-z0-9_-]+)")
        .expect("valid permission entry regex")
});

static TOP_LEVEL_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_-]*\s*:").expect("valid top-level yaml key regex")
});

static YAML_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[A-Za-z_][A-Za-z0-9_-]*\s*:").expect("valid yaml key regex"));

static JOB_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^  (?P<name>[A-Za-z0-9_-]+):\s*(?:#.*)?$").expect("valid job line regex")
});

static JOB_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+(?P<field>[A-Za-z0-9_-]+):").expect("valid job field regex"));

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PolicyOptions {
    pub repo_root: PathBuf,
    pub workflows_path: PathBuf,
    pub check_scripts: bool,
    pub check_policies: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PolicyReport {
    pub workflow_files: usize,
    pub script_usages: Vec<ScriptUsage>,
    pub policy_violations: Vec<PolicyViolation>,
    pub summary: PolicySummary,
}

impl PolicyReport {
    #[must_use]
    pub const fn has_findings(&self) -> bool {
        !self.script_usages.is_empty() || !self.policy_violations.is_empty()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct PolicySummary {
    pub total_scripts: usize,
    pub bash_scripts: usize,
    pub python_scripts: usize,
    pub high_violations: usize,
    pub medium_violations: usize,
    pub low_violations: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PolicyScanner;

impl PolicyScanner {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub fn scan(&self, options: &PolicyOptions) -> Result<PolicyReport> {
        let repo_root = options.repo_root.canonicalize().with_context(|| {
            format!("failed to resolve repository root '{}'", options.repo_root.display())
        })?;
        let workflow_root = if options.workflows_path.is_absolute() {
            options.workflows_path.clone()
        } else {
            repo_root.join(&options.workflows_path)
        };

        if !workflow_root.exists() {
            return Ok(PolicyReport {
                workflow_files: 0,
                script_usages: Vec::new(),
                policy_violations: Vec::new(),
                summary: PolicySummary::default(),
            });
        }

        let workflow_files = discover_workflow_files(&repo_root, &options.workflows_path)?;
        let mut script_usages = Vec::new();
        let mut policy_violations = Vec::new();

        for workflow_file in &workflow_files {
            if options.check_scripts {
                script_usages.extend(scan_workflow_scripts(workflow_file)?);
            }
            if options.check_policies {
                policy_violations.extend(scan_policy_violations(workflow_file)?);
            }
        }

        let summary = summarize(&script_usages, &policy_violations);

        Ok(PolicyReport {
            workflow_files: workflow_files.len(),
            script_usages,
            policy_violations,
            summary,
        })
    }
}

fn scan_policy_violations(workflow_file: &std::path::Path) -> Result<Vec<PolicyViolation>> {
    let mut violations = Vec::new();

    for action in scan_workflow(workflow_file)? {
        if !action.is_pinned() {
            violations.push(PolicyViolation {
                file: action.file,
                line_number: action.line_number,
                violation_type: PolicyViolationType::UnpinnedAction,
                severity: PolicySeverity::High,
                description: format!(
                    "external action '{}' should be pinned to a full 40-character commit SHA",
                    action.action_slug
                ),
                context: action.original_line,
            });
        }
    }

    let content = fs::read_to_string(workflow_file)
        .with_context(|| format!("failed to read workflow '{}'", workflow_file.display()))?;
    violations.extend(scan_permission_violations(workflow_file, &content));
    violations.extend(scan_timeout_violations(workflow_file, &content));

    Ok(violations)
}

fn scan_permission_violations(path: &std::path::Path, content: &str) -> Vec<PolicyViolation> {
    let mut violations = Vec::new();
    let mut has_top_level_permissions = false;
    let mut permissions_block_indent = None;

    for (index, line) in content.lines().enumerate() {
        let line_number = index + 1;
        if let Some(block_indent) = permissions_block_indent {
            if !line.trim().is_empty()
                && leading_spaces(line) <= block_indent
                && YAML_KEY_RE.is_match(line)
            {
                permissions_block_indent = None;
            } else if let Some(captures) = PERMISSION_ENTRY_RE.captures(line) {
                let scope = captures.name("scope").expect("scope capture is required").as_str();
                let value = captures.name("value").expect("value capture is required").as_str();
                push_permission_value_violation(
                    path,
                    line_number,
                    scope,
                    value,
                    line,
                    &mut violations,
                );
            }
        }

        let Some(captures) = PERMISSIONS_LINE_RE.captures(line) else {
            continue;
        };

        let indent = captures.name("indent").map_or(0, |capture| capture.as_str().chars().count());
        if indent == 0 {
            has_top_level_permissions = true;
        }

        let value = captures.name("value").expect("value capture is required").as_str().trim();
        if value.is_empty() {
            permissions_block_indent = Some(indent);
        } else {
            push_permissions_shorthand_violation(path, line_number, value, line, &mut violations);
        }
    }

    if !has_top_level_permissions {
        violations.push(PolicyViolation {
            file: path.to_path_buf(),
            line_number: 1,
            violation_type: PolicyViolationType::MissingPermissions,
            severity: PolicySeverity::Medium,
            description: "workflow should declare explicit top-level permissions".to_owned(),
            context: String::new(),
        });
    }

    violations
}

fn push_permissions_shorthand_violation(
    path: &std::path::Path,
    line_number: usize,
    value: &str,
    context: &str,
    violations: &mut Vec<PolicyViolation>,
) {
    match value {
        "write-all" => violations.push(PolicyViolation {
            file: path.to_path_buf(),
            line_number,
            violation_type: PolicyViolationType::ExcessivePermissions,
            severity: PolicySeverity::High,
            description: "permissions should not use write-all".to_owned(),
            context: context.to_owned(),
        }),
        "read-all" | "{}" | "read" => {}
        other if other.contains("write") => violations.push(PolicyViolation {
            file: path.to_path_buf(),
            line_number,
            violation_type: PolicyViolationType::ExcessivePermissions,
            severity: PolicySeverity::Medium,
            description: format!("permission shorthand '{other}' should be reviewed"),
            context: context.to_owned(),
        }),
        _ => {}
    }
}

fn push_permission_value_violation(
    path: &std::path::Path,
    line_number: usize,
    scope: &str,
    value: &str,
    context: &str,
    violations: &mut Vec<PolicyViolation>,
) {
    if value != "write" {
        return;
    }

    violations.push(PolicyViolation {
        file: path.to_path_buf(),
        line_number,
        violation_type: PolicyViolationType::ExcessivePermissions,
        severity: if scope == "id-token" { PolicySeverity::Low } else { PolicySeverity::Medium },
        description: format!("permission '{scope}: write' should be minimized or justified"),
        context: context.to_owned(),
    });
}

fn scan_timeout_violations(path: &std::path::Path, content: &str) -> Vec<PolicyViolation> {
    let mut violations = Vec::new();
    let mut in_jobs = false;
    let mut current_job = None;

    for (index, line) in content.lines().enumerate() {
        let line_number = index + 1;
        if line.starts_with("jobs:") {
            in_jobs = true;
            continue;
        }

        if in_jobs && TOP_LEVEL_KEY_RE.is_match(line) && !line.starts_with("jobs:") {
            finish_job(path, current_job.take(), &mut violations);
            in_jobs = false;
        }

        if !in_jobs {
            continue;
        }

        if let Some(captures) = JOB_LINE_RE.captures(line) {
            finish_job(path, current_job.take(), &mut violations);
            let name = captures.name("name").expect("name capture is required").as_str().to_owned();
            current_job = Some(JobState {
                name,
                line_number,
                context: line.to_owned(),
                has_timeout: false,
                has_runnable_content: false,
            });
            continue;
        }

        if let Some(job) = &mut current_job
            && let Some(captures) = JOB_FIELD_RE.captures(line)
        {
            let field = captures.name("field").expect("field capture is required").as_str();
            match field {
                "timeout-minutes" => job.has_timeout = true,
                "runs-on" | "steps" => job.has_runnable_content = true,
                _ => {}
            }
        }
    }

    finish_job(path, current_job, &mut violations);

    violations
}

fn finish_job(
    path: &std::path::Path,
    job: Option<JobState>,
    violations: &mut Vec<PolicyViolation>,
) {
    let Some(job) = job else {
        return;
    };

    if job.has_runnable_content && !job.has_timeout {
        violations.push(PolicyViolation {
            file: path.to_path_buf(),
            line_number: job.line_number,
            violation_type: PolicyViolationType::MissingTimeoutMinutes,
            severity: PolicySeverity::Medium,
            description: format!("job '{}' should declare timeout-minutes", job.name),
            context: job.context,
        });
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct JobState {
    name: String,
    line_number: usize,
    context: String,
    has_timeout: bool,
    has_runnable_content: bool,
}

fn summarize(scripts: &[ScriptUsage], violations: &[PolicyViolation]) -> PolicySummary {
    PolicySummary {
        total_scripts: scripts.len(),
        bash_scripts: scripts.iter().filter(|usage| usage.script_type == ScriptType::Bash).count(),
        python_scripts: scripts
            .iter()
            .filter(|usage| usage.script_type == ScriptType::Python)
            .count(),
        high_violations: violations
            .iter()
            .filter(|violation| violation.severity == PolicySeverity::High)
            .count(),
        medium_violations: violations
            .iter()
            .filter(|violation| violation.severity == PolicySeverity::Medium)
            .count(),
        low_violations: violations
            .iter()
            .filter(|violation| violation.severity == PolicySeverity::Low)
            .count(),
    }
}

fn leading_spaces(value: &str) -> usize {
    value.chars().take_while(|character| *character == ' ').count()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::{PolicyOptions, PolicyScanner};
    use crate::model::{PolicySeverity, PolicyViolationType, ScriptType};

    #[test]
    fn scanner_reports_scripts_and_policy_violations() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow_dir = temp_dir.path().join(".github").join("workflows");
        fs::create_dir_all(&workflow_dir).expect("create workflow directory");
        fs::write(
            workflow_dir.join("ci.yml"),
            r"name: CI

on:
  pull_request:

permissions:
  contents: write

jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: python3 scripts/check.py
      - run: ./bin/bootstrap.sh
",
        )
        .expect("write workflow");

        let scanner = PolicyScanner::new();
        let report = scanner
            .scan(&PolicyOptions {
                repo_root: temp_dir.path().to_path_buf(),
                workflows_path: PathBuf::from(".github/workflows"),
                check_scripts: true,
                check_policies: true,
            })
            .expect("scan workflows");

        assert_eq!(report.workflow_files, 1);
        assert_eq!(report.summary.total_scripts, 2);
        assert_eq!(report.summary.bash_scripts, 1);
        assert_eq!(report.summary.python_scripts, 1);
        assert!(report.script_usages.iter().any(|usage| usage.script_type == ScriptType::Bash));
        assert!(report.script_usages.iter().any(|usage| usage.script_type == ScriptType::Python));
        assert!(
            report
                .policy_violations
                .iter()
                .any(|violation| violation.violation_type == PolicyViolationType::UnpinnedAction)
        );
        assert!(
            report
                .policy_violations
                .iter()
                .any(|violation| violation.violation_type
                    == PolicyViolationType::ExcessivePermissions)
        );
        assert!(report.policy_violations.iter().any(
            |violation| violation.violation_type == PolicyViolationType::MissingTimeoutMinutes
        ));
    }

    #[test]
    fn scanner_treats_missing_workflow_directory_as_empty_report() {
        let temp_dir = tempdir().expect("tempdir");
        let scanner = PolicyScanner::new();
        let report = scanner
            .scan(&PolicyOptions {
                repo_root: temp_dir.path().to_path_buf(),
                workflows_path: PathBuf::from(".github/workflows"),
                check_scripts: true,
                check_policies: true,
            })
            .expect("scan workflows");

        assert_eq!(report.workflow_files, 0);
        assert!(!report.has_findings());
    }

    #[test]
    fn scanner_accepts_compliant_workflow() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow_dir = temp_dir.path().join(".github").join("workflows");
        fs::create_dir_all(&workflow_dir).expect("create workflow directory");
        fs::write(
            workflow_dir.join("ci.yml"),
            r"name: CI

on:
  pull_request:

permissions:
  contents: read

jobs:
  lint:
    timeout-minutes: 10
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@0123456789abcdef0123456789abcdef01234567 # v4
      - run: cargo test
",
        )
        .expect("write workflow");

        let scanner = PolicyScanner::new();
        let report = scanner
            .scan(&PolicyOptions {
                repo_root: temp_dir.path().to_path_buf(),
                workflows_path: PathBuf::from(".github/workflows"),
                check_scripts: true,
                check_policies: true,
            })
            .expect("scan workflows");

        assert_eq!(report.workflow_files, 1);
        assert!(!report.has_findings());
    }

    #[test]
    fn scanner_stops_job_permission_blocks_at_sibling_fields() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow_dir = temp_dir.path().join(".github").join("workflows");
        fs::create_dir_all(&workflow_dir).expect("create workflow directory");
        fs::write(
            workflow_dir.join("ci.yml"),
            r"name: CI

on:
  pull_request:

permissions:
  contents: read

jobs:
  lint:
    permissions:
      contents: read
    timeout-minutes: 10
    runs-on: ubuntu-latest
    steps:
      - run: cargo test
",
        )
        .expect("write workflow");

        let scanner = PolicyScanner::new();
        let report = scanner
            .scan(&PolicyOptions {
                repo_root: temp_dir.path().to_path_buf(),
                workflows_path: PathBuf::from(".github/workflows"),
                check_scripts: false,
                check_policies: true,
            })
            .expect("scan workflows");

        assert!(
            !report
                .policy_violations
                .iter()
                .any(|violation| violation.violation_type
                    == PolicyViolationType::ExcessivePermissions)
        );
    }

    #[test]
    fn scanner_flags_missing_top_level_permissions_and_write_all() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow_dir = temp_dir.path().join(".github").join("workflows");
        fs::create_dir_all(&workflow_dir).expect("create workflow directory");
        fs::write(
            workflow_dir.join("release.yml"),
            r"name: Release

on:
  workflow_dispatch:

jobs:
  release:
    permissions: write-all
    timeout-minutes: 20
    runs-on: ubuntu-latest
    steps:
      - run: cargo test
",
        )
        .expect("write workflow");

        let scanner = PolicyScanner::new();
        let report = scanner
            .scan(&PolicyOptions {
                repo_root: temp_dir.path().to_path_buf(),
                workflows_path: PathBuf::from(".github/workflows"),
                check_scripts: false,
                check_policies: true,
            })
            .expect("scan workflows");

        assert!(
            report.policy_violations.iter().any(
                |violation| violation.violation_type == PolicyViolationType::MissingPermissions
            )
        );
        assert!(
            report
                .policy_violations
                .iter()
                .any(|violation| violation.severity == PolicySeverity::High)
        );
    }
}
