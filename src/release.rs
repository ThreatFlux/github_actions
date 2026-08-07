//! Automated release publishing driven by conventional commits.
//!
//! The publisher analyzes commits since the last release tag through the
//! GitHub REST API, bumps the Cargo manifest version, and creates the release
//! commit, tag, and GitHub Release without shelling out to git — so it runs in
//! minimal containers. Failures after object creation but before the branch
//! ref update leave only unreachable git objects behind, which GitHub
//! garbage-collects; the window between tag creation and release creation is
//! not auto-healed and requires a manual `gh release create` for the tag.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use semver::Version;

use crate::{
    conventional::{self, BumpLevel, CommitKind, ConventionalCommit},
    github::{GitHubClient, TagInfo, TreeEntry},
    remote, versioning,
};

const MAX_TAG_PAGES: u32 = 10;
const MAX_COMPARE_PAGES: u32 = 10;
const MAX_FIRST_RELEASE_PAGES: u32 = 3;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseOptions {
    pub repo_root: PathBuf,
    pub owner: String,
    pub repo: String,
    pub base_branch: Option<String>,
    /// Forced bump level; `None` derives it from conventional commits.
    pub bump: Option<BumpLevel>,
    pub tag_prefix: String,
    pub update_major_alias: bool,
    /// Commit message template; `{version}` is replaced with the new version.
    pub commit_message: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ReleaseOutcome {
    Released,
    DryRun,
    SkippedNoReleasableChanges,
    SkippedRace,
    SkippedTagExists,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseReport {
    pub outcome: ReleaseOutcome,
    pub current_version: String,
    pub next_version: Option<String>,
    pub bump: Option<BumpLevel>,
    pub tag: Option<String>,
    pub major_alias: Option<String>,
    pub commit_sha: Option<String>,
    pub release_url: Option<String>,
    pub notes: Option<String>,
    pub commits_analyzed: usize,
    pub commit_range_truncated: bool,
    pub files_updated: Vec<PathBuf>,
}

impl ReleaseReport {
    /// Render `$GITHUB_OUTPUT` lines. Keys are always present; values are
    /// empty when the outcome did not produce them.
    pub fn github_outputs(&self, notes_file: &Path) -> String {
        let mut outputs = String::new();
        let released = self.outcome == ReleaseOutcome::Released;
        writeln!(outputs, "released={released}").expect("writing to a String cannot fail");
        writeln!(outputs, "version={}", self.next_version.as_deref().unwrap_or_default())
            .expect("writing to a String cannot fail");
        writeln!(outputs, "tag={}", self.tag.as_deref().unwrap_or_default())
            .expect("writing to a String cannot fail");
        writeln!(outputs, "release-url={}", self.release_url.as_deref().unwrap_or_default())
            .expect("writing to a String cannot fail");
        let notes_path =
            if self.notes.is_some() { notes_file.display().to_string() } else { String::new() };
        writeln!(outputs, "notes-file={notes_path}").expect("writing to a String cannot fail");
        outputs
    }
}

#[derive(Debug)]
struct PreparedRelease {
    branch: String,
    head_sha: String,
    next_version: Version,
    tag: String,
    notes: String,
    plan: versioning::VersionRewritePlan,
}

#[derive(Debug, Clone)]
pub struct ReleasePublisher {
    github: GitHubClient,
}

impl ReleasePublisher {
    #[must_use]
    pub const fn new(github: GitHubClient) -> Self {
        Self { github }
    }

    pub fn release(&self, options: &ReleaseOptions) -> Result<ReleaseReport> {
        self.github.ensure_token()?;
        let owner = &options.owner;
        let repo = &options.repo;

        let branch = match options.base_branch.as_deref().map(str::trim) {
            Some(branch) if !branch.is_empty() => branch.to_owned(),
            _ => self.github.default_branch(owner, repo)?,
        };
        let head_sha = self.github.branch_head_sha(owner, repo, &branch)?;

        let current_version = versioning::current_version(&options.repo_root)?;
        let current_parsed = Version::parse(&current_version).with_context(|| {
            format!("invalid current version '{current_version}' in Cargo.toml")
        })?;

        let last_tag = self.last_release_tag(owner, repo, &options.tag_prefix)?;
        let range = match &last_tag {
            Some(tag) => {
                self.github.compare_commits(owner, repo, &tag.name, &head_sha, MAX_COMPARE_PAGES)?
            }
            None => self.github.list_commits(owner, repo, &head_sha, MAX_FIRST_RELEASE_PAGES)?,
        };
        let commits = conventional::classify_commits(&range.commits);
        let bump = options.bump.or_else(|| conventional::required_bump(&commits));

        let mut report = ReleaseReport {
            outcome: ReleaseOutcome::SkippedNoReleasableChanges,
            current_version,
            next_version: None,
            bump,
            tag: None,
            major_alias: None,
            commit_sha: None,
            release_url: None,
            notes: None,
            commits_analyzed: commits.len(),
            commit_range_truncated: range.truncated,
            files_updated: Vec::new(),
        };

        let Some(bump_level) = bump else {
            return Ok(report);
        };

        let next_version = conventional::bump_version(&current_parsed, bump_level);
        let next = next_version.to_string();
        let tag = format!("{}{next}", options.tag_prefix);
        report.next_version = Some(next);
        report.tag = Some(tag.clone());
        report.notes = Some(generate_release_notes(&tag, &commits, range.truncated));

        if self.github.reference_sha(owner, repo, &format!("tags/{tag}"))?.is_some() {
            report.outcome = ReleaseOutcome::SkippedTagExists;
            return Ok(report);
        }

        let plan = versioning::plan_version_rewrite(
            &options.repo_root,
            report.next_version.as_deref().expect("next version was just set"),
        )?;
        report.files_updated = plan.file_updates.iter().map(|update| update.file.clone()).collect();

        if options.dry_run {
            report.outcome = ReleaseOutcome::DryRun;
            return Ok(report);
        }

        let prepared = PreparedRelease {
            branch,
            head_sha,
            next_version,
            tag,
            notes: report.notes.clone().expect("notes were just set"),
            plan,
        };
        self.publish(options, &prepared, &mut report)?;
        Ok(report)
    }

    fn publish(
        &self,
        options: &ReleaseOptions,
        prepared: &PreparedRelease,
        report: &mut ReleaseReport,
    ) -> Result<()> {
        let owner = &options.owner;
        let repo = &options.repo;
        let repo_root = options.repo_root.canonicalize().with_context(|| {
            format!("failed to resolve repository root '{}'", options.repo_root.display())
        })?;

        let base_tree_sha = self.github.commit_tree_sha(owner, repo, &prepared.head_sha)?;
        let mut tree_entries = Vec::new();
        for update in &prepared.plan.file_updates {
            let path = remote::relative_repository_path(&repo_root, &update.file)?;
            let blob_sha = self.github.create_blob(owner, repo, &update.updated_content)?;
            tree_entries.push(TreeEntry { path, sha: blob_sha });
        }
        let tree_sha = self.github.create_tree(owner, repo, &base_tree_sha, &tree_entries)?;
        let next = prepared.next_version.to_string();
        let commit_message = options.commit_message.replace("{version}", &next);
        let commit_sha = self.github.create_commit(
            owner,
            repo,
            &commit_message,
            &tree_sha,
            &prepared.head_sha,
        )?;

        // Cheap race check first for a clear skip, then the authoritative
        // compare-and-swap: a non-fast-forward ref update means the branch
        // advanced past the analyzed head.
        let current_head = self.github.branch_head_sha(owner, repo, &prepared.branch)?;
        if current_head != prepared.head_sha
            || !self.github.update_ref_fast_forward(
                owner,
                repo,
                &format!("heads/{}", prepared.branch),
                &commit_sha,
            )?
        {
            report.outcome = ReleaseOutcome::SkippedRace;
            return Ok(());
        }
        report.commit_sha = Some(commit_sha.clone());

        self.github.create_ref(owner, repo, &format!("tags/{}", prepared.tag), &commit_sha)?;

        if options.update_major_alias {
            let alias = format!("{}{}", options.tag_prefix, prepared.next_version.major);
            let alias_ref = format!("tags/{alias}");
            if self.github.reference_sha(owner, repo, &alias_ref)?.is_some() {
                self.github.update_ref(owner, repo, &alias_ref, &commit_sha, true)?;
            } else {
                self.github.create_ref(owner, repo, &alias_ref, &commit_sha)?;
            }
            report.major_alias = Some(alias);
        }

        let release = self.github.create_release(
            owner,
            repo,
            &prepared.tag,
            &format!("Release {}", prepared.tag),
            &prepared.notes,
            &commit_sha,
        )?;
        report.release_url = Some(release.url);
        report.outcome = ReleaseOutcome::Released;
        Ok(())
    }

    fn last_release_tag(
        &self,
        owner: &str,
        repo: &str,
        tag_prefix: &str,
    ) -> Result<Option<TagInfo>> {
        let tags = self.github.list_tags(owner, repo, MAX_TAG_PAGES)?;
        Ok(tags
            .into_iter()
            .filter_map(|tag| {
                let version = tag.name.strip_prefix(tag_prefix)?;
                let parsed = Version::parse(version).ok()?;
                Some((parsed, tag))
            })
            .max_by(|(left, _), (right, _)| left.cmp(right))
            .map(|(_, tag)| tag))
    }
}

fn generate_release_notes(tag: &str, commits: &[ConventionalCommit], truncated: bool) -> String {
    let mut notes = format!("## Release {tag}\n");
    let sections = [
        (CommitKind::Breaking, "Breaking Changes"),
        (CommitKind::Feature, "Features"),
        (CommitKind::Fix, "Bug Fixes"),
    ];

    for (kind, title) in sections {
        let entries = commits.iter().filter(|commit| commit.kind == kind);
        let mut header_written = false;
        for commit in entries {
            if !header_written {
                notes.push('\n');
                writeln!(notes, "### {title}").expect("writing to a String cannot fail");
                header_written = true;
            }
            let short_sha = commit.sha.get(..7).unwrap_or(&commit.sha);
            writeln!(notes, "- {} ({short_sha})", commit.subject)
                .expect("writing to a String cannot fail");
        }
    }

    if truncated {
        notes.push('\n');
        writeln!(notes, "_Note: the commit list was truncated; some changes may be missing._")
            .expect("writing to a String cannot fail");
    }

    notes
}

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
mod tests {
    use std::{fs, path::Path};

    use mockito::{Matcher, Server, ServerGuard};
    use tempfile::{TempDir, tempdir};

    use super::{ReleaseOptions, ReleaseOutcome, ReleasePublisher, generate_release_notes};
    use crate::{
        GitHubClient,
        conventional::{BumpLevel, classify_commits},
        github::CommitInfo,
    };

    fn write_fixture_repo() -> TempDir {
        let temp_dir = tempdir().expect("tempdir");
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.2.3\"\n",
        )
        .expect("write Cargo.toml");
        temp_dir
    }

    fn options(repo_root: &Path) -> ReleaseOptions {
        ReleaseOptions {
            repo_root: repo_root.to_path_buf(),
            owner: String::from("acme"),
            repo: String::from("demo"),
            base_branch: Some(String::from("main")),
            bump: None,
            tag_prefix: String::from("v"),
            update_major_alias: false,
            commit_message: String::from("chore: release v{version}"),
            dry_run: false,
        }
    }

    fn publisher(server: &ServerGuard) -> ReleasePublisher {
        let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
            .expect("github client");
        ReleasePublisher::new(client)
    }

    fn mock_head_ref(server: &mut ServerGuard, sha: &str, hits: usize) -> mockito::Mock {
        server
            .mock("GET", "/repos/acme/demo/git/ref/heads/main")
            .expect(hits)
            .with_status(200)
            .with_body(format!(r#"{{"ref":"refs/heads/main","object":{{"sha":"{sha}"}}}}"#))
            .create()
    }

    fn mock_tags(server: &mut ServerGuard, body: &str) -> mockito::Mock {
        server
            .mock("GET", "/repos/acme/demo/tags?per_page=100&page=1")
            .with_status(200)
            .with_body(body)
            .create()
    }

    fn mock_compare(server: &mut ServerGuard, body: &str) -> mockito::Mock {
        server
            .mock("GET", "/repos/acme/demo/compare/v0.2.3...basecommitsha?per_page=100&page=1")
            .with_status(200)
            .with_body(body)
            .create()
    }

    const FEAT_AND_FIX: &str = r#"{"total_commits":2,"commits":[{"sha":"feataaaaaaa","commit":{"message":"feat: add thing"},"parents":[{}]},{"sha":"fixbbbbbbbb","commit":{"message":"fix: repair thing"},"parents":[{}]}]}"#;
    const TAGS_V023: &str = r#"[{"name":"v0.2.3","commit":{"sha":"tagsha"}}]"#;

    #[test]
    fn release_creates_commit_tag_and_release() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        let _head = mock_head_ref(&mut server, "basecommitsha", 2);
        let _tags = mock_tags(&mut server, TAGS_V023);
        let _compare = mock_compare(&mut server, FEAT_AND_FIX);
        let _tag_missing = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0.3.0")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();
        let _base_commit = server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"sha":"basecommitsha","tree":{"sha":"basetreesha"}}"#)
            .create();
        let _blob = server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .match_body(Matcher::Regex(r#"version = \\"0\.3\.0\\""#.into()))
            .expect(1)
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create();
        let _tree = server
            .mock("POST", "/repos/acme/demo/git/trees")
            .match_body(Matcher::Regex(r#""path":"Cargo\.toml""#.into()))
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create();
        let _commit = server
            .mock("POST", "/repos/acme/demo/git/commits")
            .match_body(Matcher::Regex(r#""message":"chore: release v0\.3\.0""#.into()))
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create();
        let _advance = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .match_body(Matcher::Regex(r#""sha":"newcommitsha""#.into()))
            .with_status(200)
            .with_body(r#"{"ref":"refs/heads/main"}"#)
            .create();
        let _tag_ref = server
            .mock("POST", "/repos/acme/demo/git/refs")
            .match_body(Matcher::Regex(r#""ref":"refs/tags/v0\.3\.0""#.into()))
            .with_status(201)
            .with_body(r#"{"ref":"refs/tags/v0.3.0"}"#)
            .create();
        let _release = server
            .mock("POST", "/repos/acme/demo/releases")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#""tag_name":"v0\.3\.0""#.into()),
                Matcher::Regex(r#""target_commitish":"newcommitsha""#.into()),
                Matcher::Regex("### Features".into()),
            ]))
            .with_status(201)
            .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.3.0"}"#)
            .create();

        let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

        assert_eq!(report.outcome, ReleaseOutcome::Released);
        assert_eq!(report.current_version, "0.2.3");
        assert_eq!(report.next_version.as_deref(), Some("0.3.0"));
        assert_eq!(report.bump, Some(BumpLevel::Minor));
        assert_eq!(report.tag.as_deref(), Some("v0.3.0"));
        assert_eq!(report.commit_sha.as_deref(), Some("newcommitsha"));
        assert_eq!(
            report.release_url.as_deref(),
            Some("https://github.com/acme/demo/releases/tag/v0.3.0")
        );
        assert_eq!(report.files_updated.len(), 1);
    }

    #[test]
    fn release_dry_run_performs_no_mutations() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        let _head = mock_head_ref(&mut server, "basecommitsha", 1);
        let _tags = mock_tags(&mut server, TAGS_V023);
        let _compare = mock_compare(&mut server, FEAT_AND_FIX);
        let _tag_missing = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0.3.0")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();
        let _no_blob = server.mock("POST", "/repos/acme/demo/git/blobs").expect(0).create();
        let _no_refs = server.mock("POST", "/repos/acme/demo/git/refs").expect(0).create();
        let _no_release = server.mock("POST", "/repos/acme/demo/releases").expect(0).create();

        let mut release_options = options(temp_dir.path());
        release_options.dry_run = true;
        let report = publisher(&server).release(&release_options).expect("release report");

        assert_eq!(report.outcome, ReleaseOutcome::DryRun);
        assert_eq!(report.next_version.as_deref(), Some("0.3.0"));
        assert!(report.notes.as_deref().is_some_and(|notes| notes.contains("### Features")));
        assert_eq!(report.files_updated.len(), 1);
    }

    #[test]
    fn release_skips_when_no_commits_warrant_a_release() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        let _head = mock_head_ref(&mut server, "basecommitsha", 1);
        let _tags = mock_tags(&mut server, TAGS_V023);
        let _compare = mock_compare(
            &mut server,
            r#"{"total_commits":1,"commits":[{"sha":"choreaaaaaa","commit":{"message":"chore: tidy"},"parents":[{}]}]}"#,
        );
        let _no_blob = server.mock("POST", "/repos/acme/demo/git/blobs").expect(0).create();

        let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

        assert_eq!(report.outcome, ReleaseOutcome::SkippedNoReleasableChanges);
        assert_eq!(report.next_version, None);
        assert_eq!(report.commits_analyzed, 1);
    }

    #[test]
    fn release_bump_override_releases_without_qualifying_commits() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        let _head = mock_head_ref(&mut server, "basecommitsha", 2);
        let _tags = mock_tags(&mut server, TAGS_V023);
        let _compare = mock_compare(
            &mut server,
            r#"{"total_commits":1,"commits":[{"sha":"choreaaaaaa","commit":{"message":"chore: tidy"},"parents":[{}]}]}"#,
        );
        let _tag_missing = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0.2.4")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();
        let _base_commit = server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"sha":"basecommitsha","tree":{"sha":"basetreesha"}}"#)
            .create();
        let _blob = server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create();
        let _tree = server
            .mock("POST", "/repos/acme/demo/git/trees")
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create();
        let _commit = server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create();
        let _advance = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .with_status(200)
            .with_body(r#"{"ref":"refs/heads/main"}"#)
            .create();
        let _tag_ref = server
            .mock("POST", "/repos/acme/demo/git/refs")
            .with_status(201)
            .with_body(r#"{"ref":"refs/tags/v0.2.4"}"#)
            .create();
        let _release = server
            .mock("POST", "/repos/acme/demo/releases")
            .with_status(201)
            .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.2.4"}"#)
            .create();

        let mut release_options = options(temp_dir.path());
        release_options.bump = Some(BumpLevel::Patch);
        let report = publisher(&server).release(&release_options).expect("release report");

        assert_eq!(report.outcome, ReleaseOutcome::Released);
        assert_eq!(report.next_version.as_deref(), Some("0.2.4"));
    }

    #[test]
    fn release_skips_when_tag_already_exists() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        let _head = mock_head_ref(&mut server, "basecommitsha", 1);
        let _tags = mock_tags(&mut server, TAGS_V023);
        let _compare = mock_compare(&mut server, FEAT_AND_FIX);
        let _tag_exists = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0.3.0")
            .with_status(200)
            .with_body(r#"{"ref":"refs/tags/v0.3.0","object":{"sha":"existingsha"}}"#)
            .create();
        let _no_blob = server.mock("POST", "/repos/acme/demo/git/blobs").expect(0).create();

        let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

        assert_eq!(report.outcome, ReleaseOutcome::SkippedTagExists);
        assert_eq!(report.tag.as_deref(), Some("v0.3.0"));
    }

    #[test]
    fn release_skips_when_branch_advances_before_the_ref_update() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        // First read pins the analysis head; the re-read sees a newer commit.
        let _initial_head = mock_head_ref(&mut server, "basecommitsha", 1);
        let _moved_head = mock_head_ref(&mut server, "advancedsha", 1);
        let _tags = mock_tags(&mut server, TAGS_V023);
        let _compare = mock_compare(&mut server, FEAT_AND_FIX);
        let _tag_missing = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0.3.0")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();
        let _base_commit = server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"sha":"basecommitsha","tree":{"sha":"basetreesha"}}"#)
            .create();
        let _blob = server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create();
        let _tree = server
            .mock("POST", "/repos/acme/demo/git/trees")
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create();
        let _commit = server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create();
        let _no_advance =
            server.mock("PATCH", "/repos/acme/demo/git/refs/heads/main").expect(0).create();
        let _no_release = server.mock("POST", "/repos/acme/demo/releases").expect(0).create();

        let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

        assert_eq!(report.outcome, ReleaseOutcome::SkippedRace);
        assert_eq!(report.commit_sha, None);
    }

    #[test]
    fn release_skips_when_the_ref_update_is_not_a_fast_forward() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        let _head = mock_head_ref(&mut server, "basecommitsha", 2);
        let _tags = mock_tags(&mut server, TAGS_V023);
        let _compare = mock_compare(&mut server, FEAT_AND_FIX);
        let _tag_missing = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0.3.0")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();
        let _base_commit = server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"sha":"basecommitsha","tree":{"sha":"basetreesha"}}"#)
            .create();
        let _blob = server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create();
        let _tree = server
            .mock("POST", "/repos/acme/demo/git/trees")
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create();
        let _commit = server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create();
        let _rejected = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .with_status(422)
            .with_body(r#"{"message":"Update is not a fast forward"}"#)
            .create();
        let _no_release = server.mock("POST", "/repos/acme/demo/releases").expect(0).create();

        let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

        assert_eq!(report.outcome, ReleaseOutcome::SkippedRace);
    }

    #[test]
    fn release_handles_repositories_without_tags() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        let _head = mock_head_ref(&mut server, "basecommitsha", 2);
        let _tags = mock_tags(&mut server, "[]");
        let _commits = server
            .mock("GET", "/repos/acme/demo/commits?sha=basecommitsha&per_page=100&page=1")
            .with_status(200)
            .with_body(
                r#"[{"sha":"feataaaaaaa","commit":{"message":"feat: initial"},"parents":[]}]"#,
            )
            .create();
        let _tag_missing = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0.3.0")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();
        let _base_commit = server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"sha":"basecommitsha","tree":{"sha":"basetreesha"}}"#)
            .create();
        let _blob = server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create();
        let _tree = server
            .mock("POST", "/repos/acme/demo/git/trees")
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create();
        let _commit = server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create();
        let _advance = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .with_status(200)
            .with_body(r#"{"ref":"refs/heads/main"}"#)
            .create();
        let _tag_ref = server
            .mock("POST", "/repos/acme/demo/git/refs")
            .with_status(201)
            .with_body(r#"{"ref":"refs/tags/v0.3.0"}"#)
            .create();
        let _release = server
            .mock("POST", "/repos/acme/demo/releases")
            .with_status(201)
            .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.3.0"}"#)
            .create();

        let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

        assert_eq!(report.outcome, ReleaseOutcome::Released);
        assert_eq!(report.next_version.as_deref(), Some("0.3.0"));
    }

    #[test]
    fn release_moves_an_existing_major_alias_with_force() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        let _head = mock_head_ref(&mut server, "basecommitsha", 2);
        let _tags = mock_tags(&mut server, TAGS_V023);
        let _compare = mock_compare(&mut server, FEAT_AND_FIX);
        let _tag_missing = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0.3.0")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();
        let _base_commit = server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"sha":"basecommitsha","tree":{"sha":"basetreesha"}}"#)
            .create();
        let _blob = server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create();
        let _tree = server
            .mock("POST", "/repos/acme/demo/git/trees")
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create();
        let _commit = server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create();
        let _advance = server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .with_status(200)
            .with_body(r#"{"ref":"refs/heads/main"}"#)
            .create();
        let _tag_ref = server
            .mock("POST", "/repos/acme/demo/git/refs")
            .match_body(Matcher::Regex(r#""ref":"refs/tags/v0\.3\.0""#.into()))
            .expect(1)
            .with_status(201)
            .with_body(r#"{"ref":"refs/tags/v0.3.0"}"#)
            .create();
        let _alias_exists = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0")
            .with_status(200)
            .with_body(r#"{"ref":"refs/tags/v0","object":{"sha":"oldaliassha"}}"#)
            .create();
        let _alias_moved = server
            .mock("PATCH", "/repos/acme/demo/git/refs/tags/v0")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#""sha":"newcommitsha""#.into()),
                Matcher::Regex(r#""force":true"#.into()),
            ]))
            .expect(1)
            .with_status(200)
            .with_body(r#"{"ref":"refs/tags/v0"}"#)
            .create();
        let _release = server
            .mock("POST", "/repos/acme/demo/releases")
            .with_status(201)
            .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.3.0"}"#)
            .create();

        let mut release_options = options(temp_dir.path());
        release_options.update_major_alias = true;
        let report = publisher(&server).release(&release_options).expect("release report");

        assert_eq!(report.outcome, ReleaseOutcome::Released);
        assert_eq!(report.major_alias.as_deref(), Some("v0"));
    }

    #[test]
    fn release_notes_group_sections_and_short_shas() {
        let commits = classify_commits(&[
            CommitInfo {
                sha: String::from("aaaaaaaaaaaaaaaaaaaa"),
                message: String::from("feat!: drop old flags"),
                is_merge: false,
            },
            CommitInfo {
                sha: String::from("bbbbbbbbbbbbbbbbbbbb"),
                message: String::from("feat: add release command"),
                is_merge: false,
            },
            CommitInfo {
                sha: String::from("cccccccccccccccccccc"),
                message: String::from("fix: handle empty tags"),
                is_merge: false,
            },
            CommitInfo {
                sha: String::from("dddddddddddddddddddd"),
                message: String::from("chore: noise"),
                is_merge: false,
            },
        ]);

        let notes = generate_release_notes("v1.0.0", &commits, false);

        assert_eq!(
            notes,
            "## Release v1.0.0\n\n### Breaking Changes\n- feat!: drop old flags (aaaaaaa)\n\n### Features\n- feat: add release command (bbbbbbb)\n\n### Bug Fixes\n- fix: handle empty tags (ccccccc)\n"
        );
    }

    #[test]
    fn release_notes_flag_truncated_commit_ranges() {
        let notes = generate_release_notes("v1.0.0", &[], true);

        assert!(notes.contains("truncated"), "{notes}");
    }

    #[test]
    fn github_outputs_render_release_and_skip_shapes() {
        let temp_dir = write_fixture_repo();
        let mut server = Server::new();
        let _head = mock_head_ref(&mut server, "basecommitsha", 1);
        let _tags = mock_tags(&mut server, TAGS_V023);
        let _compare = mock_compare(&mut server, FEAT_AND_FIX);
        let _tag_missing = server
            .mock("GET", "/repos/acme/demo/git/ref/tags/v0.3.0")
            .with_status(404)
            .with_body(r#"{"message":"Not Found"}"#)
            .create();

        let mut release_options = options(temp_dir.path());
        release_options.dry_run = true;
        let report = publisher(&server).release(&release_options).expect("release report");

        let outputs = report.github_outputs(Path::new("release_notes.md"));

        assert_eq!(
            outputs,
            "released=false\nversion=0.3.0\ntag=v0.3.0\nrelease-url=\nnotes-file=release_notes.md\n"
        );
    }
}
