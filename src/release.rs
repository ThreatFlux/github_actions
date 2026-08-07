//! Automated release publishing driven by conventional commits.
//!
//! The publisher analyzes commits since the last release tag through the
//! GitHub REST API, bumps the Cargo manifest version, and creates the release
//! commit, tag, and GitHub Release without shelling out to git — so it runs in
//! minimal containers. Failures after object creation but before the branch
//! ref update leave only unreachable git objects behind, which GitHub
//! garbage-collects; the window between tag creation and release creation is
//! not auto-healed and requires a manual `gh release create` for the tag.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use semver::Version;

use crate::{
    conventional::{self, BumpLevel, ConventionalCommit},
    github::{GitHubClient, TreeEntry},
    remote, versioning,
};

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
        let notes_path =
            if self.notes.is_some() { notes_file.display().to_string() } else { String::new() };
        format!(
            "released={}\nversion={}\ntag={}\nrelease-url={}\nnotes-file={notes_path}\n",
            self.outcome == ReleaseOutcome::Released,
            self.next_version.as_deref().unwrap_or_default(),
            self.tag.as_deref().unwrap_or_default(),
            self.release_url.as_deref().unwrap_or_default(),
        )
    }
}

#[derive(Debug)]
struct Analysis {
    branch: String,
    head_sha: String,
    current_version: String,
    current: Version,
    commits: Vec<ConventionalCommit>,
    truncated: bool,
}

#[derive(Debug)]
struct PreparedRelease {
    branch: String,
    head_sha: String,
    next_version: Version,
    tag: String,
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
        let analysis = self.analyze(options)?;
        let bump = options.bump.or_else(|| conventional::required_bump(&analysis.commits));
        let mut report = initial_report(&analysis, bump);

        let Some(bump_level) = bump else {
            return Ok(report);
        };

        let next_version = conventional::bump_version(&analysis.current, bump_level);
        let next = next_version.to_string();
        let tag = format!("{}{next}", options.tag_prefix);
        report.notes =
            Some(conventional::release_notes(&tag, &analysis.commits, analysis.truncated));
        report.next_version = Some(next);
        report.tag = Some(tag.clone());

        if self
            .github
            .reference_sha(&options.owner, &options.repo, &format!("tags/{tag}"))?
            .is_some()
        {
            report.outcome = ReleaseOutcome::SkippedTagExists;
            return Ok(report);
        }

        let plan = versioning::plan_version_rewrite(&options.repo_root, &next_version.to_string())?;
        report.files_updated = plan.file_updates.iter().map(|update| update.file.clone()).collect();

        if options.dry_run {
            report.outcome = ReleaseOutcome::DryRun;
            return Ok(report);
        }

        let prepared = PreparedRelease {
            branch: analysis.branch,
            head_sha: analysis.head_sha,
            next_version,
            tag,
            plan,
        };
        self.publish(options, &prepared, &mut report)?;
        Ok(report)
    }

    fn analyze(&self, options: &ReleaseOptions) -> Result<Analysis> {
        let owner = &options.owner;
        let repo = &options.repo;
        let branch = match options.base_branch.as_deref().map(str::trim) {
            Some(branch) if !branch.is_empty() => branch.to_owned(),
            _ => self.github.default_branch(owner, repo)?,
        };
        let head_sha = self.github.branch_head_sha(owner, repo, &branch)?;

        let current_version = versioning::current_version(&options.repo_root)?;
        let current = Version::parse(&current_version).with_context(|| {
            format!("invalid current version '{current_version}' in Cargo.toml")
        })?;

        let last_tag = self.github.latest_semver_tag(owner, repo, &options.tag_prefix)?;
        let range = match &last_tag {
            Some(tag) => {
                self.github.compare_commits(owner, repo, &tag.name, &head_sha, MAX_COMPARE_PAGES)?
            }
            None => self.github.list_commits(owner, repo, &head_sha, MAX_FIRST_RELEASE_PAGES)?,
        };

        Ok(Analysis {
            branch,
            head_sha,
            current_version,
            current,
            commits: conventional::classify_commits(&range.commits),
            truncated: range.truncated,
        })
    }

    /// Publish the prepared release. The cheap head re-read gives a clear
    /// skip; the fast-forward-only ref update is the authoritative
    /// compare-and-swap against a branch that advanced mid-run.
    fn publish(
        &self,
        options: &ReleaseOptions,
        prepared: &PreparedRelease,
        report: &mut ReleaseReport,
    ) -> Result<()> {
        let commit_sha = self.build_commit(options, prepared)?;

        let current_head =
            self.github.branch_head_sha(&options.owner, &options.repo, &prepared.branch)?;
        if current_head != prepared.head_sha
            || !self.github.update_ref_fast_forward(
                &options.owner,
                &options.repo,
                &format!("heads/{}", prepared.branch),
                &commit_sha,
            )?
        {
            report.outcome = ReleaseOutcome::SkippedRace;
            return Ok(());
        }
        report.commit_sha = Some(commit_sha.clone());

        self.finalize(options, prepared, &commit_sha, report)
    }

    fn build_commit(&self, options: &ReleaseOptions, prepared: &PreparedRelease) -> Result<String> {
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

        let message =
            options.commit_message.replace("{version}", &prepared.next_version.to_string());
        self.github.create_commit(owner, repo, &message, &tree_sha, &prepared.head_sha)
    }

    fn finalize(
        &self,
        options: &ReleaseOptions,
        prepared: &PreparedRelease,
        commit_sha: &str,
        report: &mut ReleaseReport,
    ) -> Result<()> {
        let owner = &options.owner;
        let repo = &options.repo;
        self.github.create_ref(owner, repo, &format!("tags/{}", prepared.tag), commit_sha)?;

        if options.update_major_alias {
            let alias = format!("{}{}", options.tag_prefix, prepared.next_version.major);
            let alias_ref = format!("tags/{alias}");
            if self.github.reference_sha(owner, repo, &alias_ref)?.is_some() {
                self.github.update_ref(owner, repo, &alias_ref, commit_sha, true)?;
            } else {
                self.github.create_ref(owner, repo, &alias_ref, commit_sha)?;
            }
            report.major_alias = Some(alias);
        }

        let notes = report.notes.clone().unwrap_or_default();
        let release = self.github.create_release(
            owner,
            repo,
            &prepared.tag,
            &format!("Release {}", prepared.tag),
            &notes,
            commit_sha,
        )?;
        report.release_url = Some(release.url);
        report.outcome = ReleaseOutcome::Released;
        Ok(())
    }
}

fn initial_report(analysis: &Analysis, bump: Option<BumpLevel>) -> ReleaseReport {
    ReleaseReport {
        outcome: ReleaseOutcome::SkippedNoReleasableChanges,
        current_version: analysis.current_version.clone(),
        next_version: None,
        bump,
        tag: None,
        major_alias: None,
        commit_sha: None,
        release_url: None,
        notes: None,
        commits_analyzed: analysis.commits.len(),
        commit_range_truncated: analysis.truncated,
        files_updated: Vec::new(),
    }
}

// Tests live in a sibling file to keep this module within the repository's
// file-size lint budget; they remain `super::`-scoped unit tests.
#[cfg(test)]
#[path = "release_tests.rs"]
#[allow(clippy::significant_drop_tightening)]
mod tests;
