use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::{
    github::GitHubClient,
    model::{PinChange, PinReport},
    workflow::{apply_changes, discover_workflow_files, scan_workflow},
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PinMode {
    Apply,
    DryRun,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PinOptions {
    pub repo_root: PathBuf,
    pub workflows_path: PathBuf,
    pub mode: PinMode,
}

#[derive(Debug, Clone)]
pub struct WorkflowPinner {
    github: GitHubClient,
}

impl WorkflowPinner {
    #[must_use]
    pub const fn new(github: GitHubClient) -> Self {
        Self { github }
    }

    pub fn pin(&self, options: &PinOptions) -> Result<PinReport> {
        let repo_root = options.repo_root.canonicalize().with_context(|| {
            format!("failed to resolve repository root '{}'", options.repo_root.display())
        })?;
        let workflow_files = discover_workflow_files(&repo_root, &options.workflows_path)?;

        let mut references_scanned = 0usize;
        let mut already_pinned = 0usize;
        let mut changes = Vec::new();

        for workflow_file in &workflow_files {
            for action in scan_workflow(workflow_file)? {
                references_scanned += 1;

                if action.is_pinned() {
                    already_pinned += 1;
                    continue;
                }

                let commit_sha = self.github.resolve_reference(
                    &action.owner,
                    &action.repository,
                    &action.version,
                )?;

                changes.push(PinChange {
                    file: action.file.clone(),
                    line_number: action.line_number,
                    action_slug: action.action_slug.clone(),
                    from_version: action.version.clone(),
                    to_sha: commit_sha.clone(),
                    original_line: action.original_line.clone(),
                    rewritten_line: action.rendered_line(&commit_sha, &action.version),
                });
            }
        }

        if options.mode == PinMode::Apply && !changes.is_empty() {
            apply_changes(&changes)?;
        }

        Ok(PinReport {
            workflow_files: workflow_files.len(),
            references_scanned,
            already_pinned,
            changes,
        })
    }
}

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
mod tests {
    use std::fs;

    use mockito::Server;
    use tempfile::tempdir;

    use super::{PinMode, PinOptions, WorkflowPinner};
    use crate::github::GitHubClient;

    #[test]
    fn pin_dry_run_reports_changes_without_rewriting_files() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow_dir = temp_dir.path().join(".github").join("workflows");
        fs::create_dir_all(&workflow_dir).expect("create workflow directory");
        let workflow = workflow_dir.join("ci.yml");
        fs::write(
            &workflow,
            "steps:\n  - uses: actions/checkout@v4\n  - uses: actions/cache@668228422ae6a00e4ad889ee87cd7109ec5666a7  # v5.0.4\n",
        )
        .expect("write workflow");

        let mut server = Server::new();
        let _checkout = server
            .mock("GET", "/repos/actions/checkout/commits/v4")
            .with_status(200)
            .with_body(r#"{"sha":"de0fac2e4500dabe0009e67214ff5f5447ce83dd"}"#)
            .create();

        let github = GitHubClient::new(server.url(), None).expect("github client");
        let report = WorkflowPinner::new(github)
            .pin(&PinOptions {
                repo_root: temp_dir.path().to_path_buf(),
                workflows_path: ".github/workflows".into(),
                mode: PinMode::DryRun,
            })
            .expect("pin workflows");

        assert_eq!(report.workflow_files, 1);
        assert_eq!(report.references_scanned, 2);
        assert_eq!(report.already_pinned, 1);
        assert_eq!(report.changes.len(), 1);

        let content = fs::read_to_string(&workflow).expect("read workflow");
        assert!(content.contains("actions/checkout@v4"));
    }

    #[test]
    fn pin_apply_rewrites_workflow_files() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow_dir = temp_dir.path().join(".github").join("workflows");
        fs::create_dir_all(&workflow_dir).expect("create workflow directory");
        let workflow = workflow_dir.join("release.yml");
        fs::write(&workflow, "steps:\n  - uses: github/codeql-action/init@v3 # security scan\n")
            .expect("write workflow");

        let mut server = Server::new();
        let _codeql = server
            .mock("GET", "/repos/github/codeql-action/commits/v3")
            .with_status(200)
            .with_body(r#"{"sha":"3d8036cf7fe7433e4a725cf513a6ea56c7fd0f14"}"#)
            .create();

        let github = GitHubClient::new(server.url(), None).expect("github client");
        let report = WorkflowPinner::new(github)
            .pin(&PinOptions {
                repo_root: temp_dir.path().to_path_buf(),
                workflows_path: ".github/workflows".into(),
                mode: PinMode::Apply,
            })
            .expect("pin workflows");

        assert_eq!(report.changes.len(), 1);

        let content = fs::read_to_string(&workflow).expect("read workflow");
        assert!(content.contains(
            "  - uses: github/codeql-action/init@3d8036cf7fe7433e4a725cf513a6ea56c7fd0f14  # v3 | security scan"
        ));
    }
}
