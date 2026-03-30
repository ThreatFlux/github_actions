use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::{
    github::GitHubClient,
    model::PinChange,
    workflow::{apply_changes, discover_workflow_files, scan_workflow},
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum UpdateMode {
    Apply,
    DryRun,
    Status,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UpdateOptions {
    pub repo_root: PathBuf,
    pub workflows_path: PathBuf,
    pub mode: UpdateMode,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct VersionEntry {
    pub file: PathBuf,
    pub line_number: usize,
    pub action_slug: String,
    pub pinned: bool,
    pub current_version: String,
    pub current_sha: Option<String>,
    pub latest_version: String,
    pub latest_sha: String,
    pub update_needed: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UpdateReport {
    pub workflow_files: usize,
    pub references_scanned: usize,
    pub already_pinned: usize,
    pub entries: Vec<VersionEntry>,
    pub changes: Vec<PinChange>,
}

impl UpdateReport {
    #[must_use]
    pub fn changed_files(&self) -> usize {
        let mut files = self.changes.iter().map(|change| change.file.as_path()).collect::<Vec<_>>();
        files.sort();
        files.dedup();
        files.len()
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowUpdater {
    github: GitHubClient,
}

impl WorkflowUpdater {
    #[must_use]
    pub const fn new(github: GitHubClient) -> Self {
        Self { github }
    }

    pub fn update(&self, options: &UpdateOptions) -> Result<UpdateReport> {
        let repo_root = options.repo_root.canonicalize().with_context(|| {
            format!("failed to resolve repository root '{}'", options.repo_root.display())
        })?;
        let workflow_files = discover_workflow_files(&repo_root, &options.workflows_path)?;

        let mut references_scanned = 0usize;
        let mut already_pinned = 0usize;
        let mut entries = Vec::new();
        let mut changes = Vec::new();

        for workflow_file in &workflow_files {
            for action in scan_workflow(workflow_file)? {
                references_scanned += 1;
                if action.is_pinned() {
                    already_pinned += 1;
                }

                let latest = self.github.latest_reference(&action.owner, &action.repository)?;
                let current_version = action.logical_version();
                let current_sha = action.is_pinned().then(|| action.version.clone());
                let update_needed = !action.is_pinned()
                    || current_sha.as_deref() != Some(latest.sha.as_str())
                    || current_version != latest.version;

                entries.push(VersionEntry {
                    file: action.file.clone(),
                    line_number: action.line_number,
                    action_slug: action.action_slug.clone(),
                    pinned: action.is_pinned(),
                    current_version: current_version.clone(),
                    current_sha,
                    latest_version: latest.version.clone(),
                    latest_sha: latest.sha.clone(),
                    update_needed,
                });

                if update_needed && options.mode != UpdateMode::Status {
                    changes.push(PinChange {
                        file: action.file.clone(),
                        line_number: action.line_number,
                        action_slug: action.action_slug.clone(),
                        from_version: current_version,
                        to_sha: latest.sha.clone(),
                        original_line: action.original_line.clone(),
                        rewritten_line: action.rendered_line(&latest.sha, &latest.version),
                    });
                }
            }
        }

        if options.mode == UpdateMode::Apply && !changes.is_empty() {
            apply_changes(&changes)?;
        }

        Ok(UpdateReport {
            workflow_files: workflow_files.len(),
            references_scanned,
            already_pinned,
            entries,
            changes,
        })
    }
}

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
mod tests {
    use std::fs;

    use mockito::{Matcher, Server};
    use tempfile::tempdir;

    use crate::github::GitHubClient;

    #[test]
    fn discovers_latest_release_and_falls_back_to_tags() {
        let mut server = Server::new();
        let _latest_release = server
            .mock("GET", "/repos/actions/checkout/releases/latest")
            .with_status(200)
            .with_body(r#"{"tag_name":"v5"}"#)
            .create();
        let _latest_release_commit = server
            .mock("GET", "/repos/actions/checkout/commits/v5")
            .with_status(200)
            .with_body(r#"{"sha":"1111111111111111111111111111111111111111"}"#)
            .create();
        let _missing_release = server
            .mock("GET", "/repos/custom/action/releases/latest")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();
        let _tag_fallback = server
            .mock("GET", "/repos/custom/action/tags")
            .match_query(Matcher::UrlEncoded("per_page".into(), "1".into()))
            .with_status(200)
            .with_body(r#"[{"name":"v1.2.3","commit":{"sha":"2222222222222222222222222222222222222222"}}]"#)
            .create();

        let client = GitHubClient::new(server.url(), None).expect("github client");

        let release =
            client.latest_reference("actions", "checkout").expect("latest release reference");
        assert_eq!(release.version, "v5");
        assert_eq!(release.sha, "1111111111111111111111111111111111111111");

        let fallback = client.latest_reference("custom", "action").expect("latest tag fallback");
        assert_eq!(fallback.version, "v1.2.3");
        assert_eq!(fallback.sha, "2222222222222222222222222222222222222222");
    }

    #[test]
    fn update_dry_run_reports_latest_versions_without_rewriting() {
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
        let _checkout_release = server
            .mock("GET", "/repos/actions/checkout/releases/latest")
            .with_status(200)
            .with_body(r#"{"tag_name":"v5"}"#)
            .create();
        let _checkout_commit = server
            .mock("GET", "/repos/actions/checkout/commits/v5")
            .with_status(200)
            .with_body(r#"{"sha":"de0fac2e4500dabe0009e67214ff5f5447ce83dd"}"#)
            .create();
        let _cache_release = server
            .mock("GET", "/repos/actions/cache/releases/latest")
            .with_status(200)
            .with_body(r#"{"tag_name":"v5.0.5"}"#)
            .create();
        let _cache_commit = server
            .mock("GET", "/repos/actions/cache/commits/v5.0.5")
            .with_status(200)
            .with_body(r#"{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#)
            .create();

        let github = GitHubClient::new(server.url(), None).expect("github client");
        let planner = crate::update::WorkflowUpdater::new(github);
        let report = planner
            .update(&crate::update::UpdateOptions {
                repo_root: temp_dir.path().to_path_buf(),
                workflows_path: ".github/workflows".into(),
                mode: crate::update::UpdateMode::DryRun,
            })
            .expect("update dry run");

        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.changes.len(), 2);
        assert_eq!(report.entries[0].latest_version, "v5");
        assert_eq!(report.entries[0].current_version, "v4");
        assert!(report.entries[0].update_needed);

        let content = fs::read_to_string(&workflow).expect("read workflow");
        assert!(content.contains("actions/checkout@v4"));
    }

    #[test]
    fn status_tracks_current_and_latest_versions() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow_dir = temp_dir.path().join(".github").join("workflows");
        fs::create_dir_all(&workflow_dir).expect("create workflow directory");
        let workflow = workflow_dir.join("release.yml");
        fs::write(
            &workflow,
            "steps:\n  - uses: github/codeql-action/init@3d8036cf7fe7433e4a725cf513a6ea56c7fd0f14  # v2.25.0 | code scanning\n",
        )
        .expect("write workflow");

        let mut server = Server::new();
        let _release = server
            .mock("GET", "/repos/github/codeql-action/releases/latest")
            .with_status(200)
            .with_body(r#"{"tag_name":"v2.26.0"}"#)
            .create();
        let _commit = server
            .mock("GET", "/repos/github/codeql-action/commits/v2.26.0")
            .with_status(200)
            .with_body(r#"{"sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#)
            .create();

        let github = GitHubClient::new(server.url(), None).expect("github client");
        let planner = crate::update::WorkflowUpdater::new(github);
        let report = planner
            .update(&crate::update::UpdateOptions {
                repo_root: temp_dir.path().to_path_buf(),
                workflows_path: ".github/workflows".into(),
                mode: crate::update::UpdateMode::Status,
            })
            .expect("status report");

        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].current_version, "v2.25.0");
        assert_eq!(report.entries[0].latest_version, "v2.26.0");
        assert!(report.entries[0].pinned);
        assert!(report.entries[0].update_needed);
        assert!(report.changes.is_empty());
    }
}
