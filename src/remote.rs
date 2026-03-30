use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow, bail};

use crate::{
    github::{GitHubClient, TreeEntry},
    model::{FileUpdate, UpdateChange},
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PullRequestOptions {
    pub repo_root: PathBuf,
    pub owner: String,
    pub repo: String,
    pub base_branch: Option<String>,
    pub branch_name: Option<String>,
    pub labels: Vec<String>,
    pub title: String,
    pub commit_message: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PullRequestResult {
    pub branch_name: String,
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct RemoteUpdatePublisher {
    github: GitHubClient,
}

impl RemoteUpdatePublisher {
    #[must_use]
    pub const fn new(github: GitHubClient) -> Self {
        Self { github }
    }

    pub fn publish(
        &self,
        file_updates: &[FileUpdate],
        changes: &[UpdateChange],
        options: &PullRequestOptions,
    ) -> Result<Option<PullRequestResult>> {
        if file_updates.is_empty() {
            return Ok(None);
        }

        self.github.validate_token_scopes()?;

        let base_branch = match options.base_branch.clone() {
            Some(branch) if !branch.trim().is_empty() => branch,
            _ => self.github.default_branch(&options.owner, &options.repo)?,
        };

        let base_commit_sha =
            self.github.branch_head_sha(&options.owner, &options.repo, &base_branch)?;
        let base_tree_sha =
            self.github.commit_tree_sha(&options.owner, &options.repo, &base_commit_sha)?;
        let branch_name = options
            .branch_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(default_branch_name);

        self.github.create_branch(&options.owner, &options.repo, &branch_name, &base_commit_sha)?;

        let mut tree_entries = Vec::new();
        let grouped_updates = file_updates.iter().fold(
            BTreeMap::<&Path, &FileUpdate>::new(),
            |mut grouped, update| {
                grouped.insert(update.file.as_path(), update);
                grouped
            },
        );

        for (file, file_update) in grouped_updates {
            let relative_path = relative_repository_path(&options.repo_root, file)?;
            let blob_sha = self.github.create_blob(
                &options.owner,
                &options.repo,
                &file_update.updated_content,
            )?;

            tree_entries.push(TreeEntry { path: relative_path, sha: blob_sha });
        }

        let tree_sha = self.github.create_tree(
            &options.owner,
            &options.repo,
            &base_tree_sha,
            &tree_entries,
        )?;
        let commit_sha = self.github.create_commit(
            &options.owner,
            &options.repo,
            &options.commit_message,
            &tree_sha,
            &base_commit_sha,
        )?;
        self.github.update_branch(&options.owner, &options.repo, &branch_name, &commit_sha)?;

        let pull_request = self.github.create_pull_request(
            &options.owner,
            &options.repo,
            &options.title,
            &generate_pr_body(changes),
            &branch_name,
            &base_branch,
        )?;

        if !options.labels.is_empty() {
            self.github.add_labels(
                &options.owner,
                &options.repo,
                pull_request.number,
                &options.labels,
            )?;
        }

        Ok(Some(PullRequestResult {
            branch_name,
            number: pull_request.number,
            url: pull_request.url,
        }))
    }
}

fn default_branch_name() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("dependency-updates-{timestamp}")
}

fn relative_repository_path(repo_root: &Path, file: &Path) -> Result<String> {
    let relative = file
        .strip_prefix(repo_root)
        .map_err(|error| anyhow!("failed to derive repository-relative path: {error}"))?;
    let path = relative.to_string_lossy().replace('\\', "/");
    if path.is_empty() {
        bail!("repository-relative path is empty");
    }
    Ok(path)
}

fn generate_pr_body(changes: &[UpdateChange]) -> String {
    let mut body = String::from("This PR updates repository dependencies:\n\n");

    for change in changes {
        writeln!(
            body,
            "* {} `{}` in `{}`: {} -> {}",
            change.kind.label(),
            change.subject,
            change.file.display(),
            change.from_version,
            change.to_version
        )
        .expect("writing to a String cannot fail");
    }

    body.push_str("\n---\n");
    body.push_str("Generated automatically by github-actions-maintainer.\n");
    body
}

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
mod tests {
    use std::fs;

    use mockito::{Matcher, Server};
    use tempfile::tempdir;

    use super::{PullRequestOptions, RemoteUpdatePublisher};
    use crate::{
        github::GitHubClient,
        model::{FileUpdate, UpdateChange, UpdateChangeKind},
    };

    #[test]
    fn publish_creates_branch_commit_and_pull_request() {
        let temp_dir = tempdir().expect("tempdir");
        let repo_root = temp_dir.path().to_path_buf();
        let workflow_dir = repo_root.join(".github").join("workflows");
        fs::create_dir_all(&workflow_dir).expect("create workflow directory");
        let workflow = workflow_dir.join("ci.yml");
        fs::write(&workflow, "steps:\n  - uses: actions/checkout@v4\n").expect("write workflow");

        let mut server = Server::new();
        let _user = server
            .mock("GET", "/user")
            .match_header("authorization", Matcher::Regex("^Bearer\\s+ghp_testtoken$".into()))
            .with_status(200)
            .with_header("x-oauth-scopes", "repo, workflow")
            .with_body(r#"{"login":"octocat"}"#)
            .create();
        let _repo = server
            .mock("GET", "/repos/acme/demo")
            .with_status(200)
            .with_body(r#"{"default_branch":"main"}"#)
            .create();
        let _ref = server
            .mock("GET", "/repos/acme/demo/git/ref/heads/main")
            .with_status(200)
            .with_body(r#"{"object":{"sha":"basecommitsha"}} "#)
            .create();
        let _commit = server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"tree":{"sha":"basetreesha"}}"#)
            .create();
        let _create_branch = server
            .mock("POST", "/repos/acme/demo/git/refs")
            .with_status(201)
            .with_body(r#"{"ref":"refs/heads/github-actions-updates-test"}"#)
            .create();
        let _blob = server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .match_body(Matcher::Regex("de0fac2e4500dabe0009e67214ff5f5447ce83dd".into()))
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create();
        let _tree = server
            .mock("POST", "/repos/acme/demo/git/trees")
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create();
        let _create_commit = server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"commitsha"}"#)
            .create();
        let _update_ref = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/github-actions-updates-test")
            .with_status(200)
            .with_body(r#"{"object":{"sha":"commitsha"}}"#)
            .create();
        let _pull = server
            .mock("POST", "/repos/acme/demo/pulls")
            .with_status(201)
            .with_body(r#"{"number":42,"html_url":"https://example.test/pr/42"}"#)
            .create();
        let _labels = server
            .mock("POST", "/repos/acme/demo/issues/42/labels")
            .with_status(200)
            .with_body(r"{}")
            .create();

        let github = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let publisher = RemoteUpdatePublisher::new(github);
        let result = publisher
            .publish(
                &[FileUpdate {
                    file: workflow_dir.join("ci.yml"),
                    updated_content: String::from(
                        "steps:\n  - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd  # v4\n",
                    ),
                }],
                &[UpdateChange {
                    kind: UpdateChangeKind::GitHubAction,
                    file: workflow,
                    line_number: Some(2),
                    subject: String::from("actions/checkout"),
                    from_version: String::from("v4"),
                    to_version: String::from("de0fac2e4500dabe0009e67214ff5f5447ce83dd"),
                }],
                &PullRequestOptions {
                    repo_root,
                    owner: String::from("acme"),
                    repo: String::from("demo"),
                    base_branch: None,
                    branch_name: Some(String::from("github-actions-updates-test")),
                    labels: vec![String::from("dependencies"), String::from("security")],
                    title: String::from("Update GitHub Actions dependencies"),
                    commit_message: String::from("Update GitHub Actions dependencies"),
                },
            )
            .expect("publish remote update")
            .expect("pull request result");

        assert_eq!(result.number, 42);
        assert_eq!(result.url, "https://example.test/pr/42");
    }

    #[test]
    fn publish_supports_cargo_manifest_updates() {
        let temp_dir = tempdir().expect("tempdir");
        let repo_root = temp_dir.path().to_path_buf();
        let manifest = repo_root.join("Cargo.toml");
        fs::write(
            &manifest,
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nreqwest = \"0.12.13\"\n",
        )
        .expect("write manifest");

        let mut server = Server::new();
        let _user = server
            .mock("GET", "/user")
            .match_header("authorization", Matcher::Regex("^Bearer\\s+ghp_testtoken$".into()))
            .with_status(200)
            .with_header("x-oauth-scopes", "repo, workflow")
            .with_body(r#"{"login":"octocat"}"#)
            .create();
        let _repo = server
            .mock("GET", "/repos/acme/demo")
            .with_status(200)
            .with_body(r#"{"default_branch":"main"}"#)
            .create();
        let _ref = server
            .mock("GET", "/repos/acme/demo/git/ref/heads/main")
            .with_status(200)
            .with_body(r#"{"object":{"sha":"basecommitsha"}} "#)
            .create();
        let _commit = server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"tree":{"sha":"basetreesha"}}"#)
            .create();
        let _create_branch = server
            .mock("POST", "/repos/acme/demo/git/refs")
            .with_status(201)
            .with_body(r#"{"ref":"refs/heads/github-actions-updates-test"}"#)
            .create();
        let _blob = server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .match_body(Matcher::Regex("0\\.12\\.15".into()))
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create();
        let _tree = server
            .mock("POST", "/repos/acme/demo/git/trees")
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create();
        let _create_commit = server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"commitsha"}"#)
            .create();
        let _update_ref = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/github-actions-updates-test")
            .with_status(200)
            .with_body(r#"{"object":{"sha":"commitsha"}}"#)
            .create();
        let _pull = server
            .mock("POST", "/repos/acme/demo/pulls")
            .with_status(201)
            .with_body(r#"{"number":43,"html_url":"https://example.test/pr/43"}"#)
            .create();

        let github = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        let publisher = RemoteUpdatePublisher::new(github);
        let result = publisher
            .publish(
                &[FileUpdate {
                    file: manifest.clone(),
                    updated_content: String::from(
                        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nreqwest = \"0.12.15\"\n",
                    ),
                }],
                &[UpdateChange {
                    kind: UpdateChangeKind::CargoDependency,
                    file: manifest,
                    line_number: None,
                    subject: String::from("reqwest"),
                    from_version: String::from("0.12.13"),
                    to_version: String::from("0.12.15"),
                }],
                &PullRequestOptions {
                    repo_root,
                    owner: String::from("acme"),
                    repo: String::from("demo"),
                    base_branch: None,
                    branch_name: Some(String::from("github-actions-updates-test")),
                    labels: Vec::new(),
                    title: String::from("Update dependencies"),
                    commit_message: String::from("Update dependencies"),
                },
            )
            .expect("publish remote update")
            .expect("pull request result");

        assert_eq!(result.number, 43);
        assert_eq!(result.url, "https://example.test/pr/43");
    }
}
