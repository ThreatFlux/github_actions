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
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use semver::Version;

use crate::{
    conventional::{self, BumpLevel, ConventionalCommit},
    github::{GitHubClient, TreeEntry},
    model::FileUpdate,
    remote, versioning,
};

const MAX_COMPARE_PAGES: u32 = 10;
const MAX_FIRST_RELEASE_PAGES: u32 = 3;
const AUTOMATED_RELEASE_BRANCH_PREFIX: &str = "automation/release";

/// How the release tag is created. Annotated tags are the default: they
/// carry a tagger identity and satisfy `git cat-file -t == "tag"` provenance
/// checks. The moving major alias tag is always lightweight.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum TagStyle {
    #[default]
    Annotated,
    Lightweight,
}

/// Which part of the release a run performs.
///
/// A container action cannot pin a runtime image built from its own release,
/// because an image tagged with the version only exists once the tag does.
/// Splitting the release breaks that cycle: [`Self::Bump`] lands the version
/// bump so an image can be built from the released version, and [`Self::Tag`]
/// then pins that image and tags the result. Only the two Dockerfiles differ
/// between the image's source and the tagged tree, and they do not affect the
/// image, so the tag ships a runtime whose reported version is its own.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum ReleasePhase {
    /// Bump, tag, and publish in a single run.
    #[default]
    All,
    /// Commit the version bump and stop, leaving the version untagged.
    Bump,
    /// Tag and publish the version the manifest already holds, without bumping.
    Tag,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseOptions {
    pub repo_root: PathBuf,
    pub owner: String,
    pub repo: String,
    pub base_branch: Option<String>,
    /// Forced bump level; `None` derives it from conventional commits.
    pub bump: Option<BumpLevel>,
    pub tag_prefix: String,
    pub tag_style: TagStyle,
    pub update_major_alias: bool,
    /// Commit message template; `{version}` is replaced with the new version.
    pub commit_message: String,
    /// Create or refresh an automation-owned release branch and pull request
    /// instead of publishing directly to the base branch.
    pub create_pr: bool,
    pub release_branch: String,
    pub dry_run: bool,
    /// Extra working-tree files to stage into the release commit, on top of
    /// the manifest and lockfile rewrites. Paths already covered by the
    /// version rewrite are ignored so the rewrite stays authoritative.
    pub extra_files: Vec<PathBuf>,
    pub phase: ReleasePhase,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ReleaseOutcome {
    Released,
    /// The version bump was committed; the tag and release are still pending.
    VersionCommitted,
    PullRequestCreated,
    PullRequestUpdated,
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
    pub pull_request_number: Option<u64>,
    pub pull_request_url: Option<String>,
    pub release_branch: Option<String>,
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
            "released={}\nversion={}\ntag={}\ncommit={}\nrelease-url={}\nrelease-pr-number={}\nrelease-pr-url={}\nrelease-branch={}\nnotes-file={notes_path}\n",
            self.outcome == ReleaseOutcome::Released,
            self.next_version.as_deref().unwrap_or_default(),
            self.tag.as_deref().unwrap_or_default(),
            self.commit_sha.as_deref().unwrap_or_default(),
            self.release_url.as_deref().unwrap_or_default(),
            self.pull_request_number.map(|number| number.to_string()).unwrap_or_default(),
            self.pull_request_url.as_deref().unwrap_or_default(),
            self.release_branch.as_deref().unwrap_or_default(),
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
    file_updates: Vec<FileUpdate>,
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
        if options.create_pr {
            if options.phase != ReleasePhase::All {
                anyhow::bail!(
                    "--create-pr cannot be combined with --phase: the release pull request already separates the version bump from the tag"
                );
            }
            validate_release_branch(&options.release_branch)?;
        }
        let analysis = self.analyze(options)?;
        if options.phase == ReleasePhase::Tag {
            return self.release_manifest_version(options, analysis);
        }
        self.release_bumped_version(options, analysis)
    }

    /// Bump the manifest version from the conventional commits, then commit it.
    /// [`ReleasePhase::Bump`] stops there; otherwise the same run tags it.
    fn release_bumped_version(
        &self,
        options: &ReleaseOptions,
        analysis: Analysis,
    ) -> Result<ReleaseReport> {
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

        if self.tag_exists(options, &tag)? {
            report.outcome = ReleaseOutcome::SkippedTagExists;
            return Ok(report);
        }

        let plan = versioning::plan_version_rewrite(&options.repo_root, &next_version.to_string())?;
        let mut file_updates = plan.file_updates;
        let extra = extra_file_updates(&options.repo_root, &options.extra_files, &file_updates)?;
        file_updates.extend(extra);

        self.prepare_and_publish(options, analysis, next_version, tag, file_updates, report)
    }

    /// Tag the version the manifest already holds, without bumping it. The
    /// bump landed in an earlier [`ReleasePhase::Bump`] run, so a runtime image
    /// built from this exact version already exists and can be pinned into the
    /// commit the tag points at.
    fn release_manifest_version(
        &self,
        options: &ReleaseOptions,
        analysis: Analysis,
    ) -> Result<ReleaseReport> {
        let mut report = initial_report(&analysis, None);
        let version = analysis.current.clone();
        let tag = format!("{}{version}", options.tag_prefix);
        report.notes =
            Some(conventional::release_notes(&tag, &analysis.commits, analysis.truncated));
        report.next_version = Some(version.to_string());
        report.tag = Some(tag.clone());

        if self.tag_exists(options, &tag)? {
            report.outcome = ReleaseOutcome::SkippedTagExists;
            return Ok(report);
        }

        // No version rewrite: the manifest is already at the released version,
        // so only the caller's extra files are staged. With none to stage the
        // tag lands on the existing head instead of an empty commit.
        let file_updates = extra_file_updates(&options.repo_root, &options.extra_files, &[])?;

        self.prepare_and_publish(options, analysis, version, tag, file_updates, report)
    }

    fn prepare_and_publish(
        &self,
        options: &ReleaseOptions,
        analysis: Analysis,
        next_version: Version,
        tag: String,
        file_updates: Vec<FileUpdate>,
        mut report: ReleaseReport,
    ) -> Result<ReleaseReport> {
        report.files_updated = file_updates.iter().map(|update| update.file.clone()).collect();

        if options.dry_run {
            report.outcome = ReleaseOutcome::DryRun;
            return Ok(report);
        }

        let prepared = PreparedRelease {
            branch: analysis.branch,
            head_sha: analysis.head_sha,
            next_version,
            tag,
            file_updates,
        };
        self.publish(options, &prepared, &mut report)?;
        Ok(report)
    }

    fn tag_exists(&self, options: &ReleaseOptions, tag: &str) -> Result<bool> {
        Ok(self
            .github
            .reference_sha(&options.owner, &options.repo, &format!("tags/{tag}"))?
            .is_some())
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
        let staged = !prepared.file_updates.is_empty();
        let commit_sha =
            if staged { self.build_commit(options, prepared)? } else { prepared.head_sha.clone() };

        let current_head =
            self.github.branch_head_sha(&options.owner, &options.repo, &prepared.branch)?;
        if current_head != prepared.head_sha {
            report.outcome = ReleaseOutcome::SkippedRace;
            return Ok(());
        }
        report.commit_sha = Some(commit_sha.clone());

        if options.create_pr {
            return self.publish_pull_request(options, prepared, &commit_sha, report);
        }

        if staged
            && !self.github.update_ref_fast_forward(
                &options.owner,
                &options.repo,
                &format!("heads/{}", prepared.branch),
                &commit_sha,
            )?
        {
            report.outcome = ReleaseOutcome::SkippedRace;
            return Ok(());
        }

        if options.phase == ReleasePhase::Bump {
            report.outcome = ReleaseOutcome::VersionCommitted;
            return Ok(());
        }

        self.finalize(options, prepared, &commit_sha, report)
    }

    fn publish_pull_request(
        &self,
        options: &ReleaseOptions,
        prepared: &PreparedRelease,
        commit_sha: &str,
        report: &mut ReleaseReport,
    ) -> Result<()> {
        let owner = &options.owner;
        let repo = &options.repo;
        let branch_ref = format!("heads/{}", options.release_branch);
        self.update_release_branch(owner, repo, &branch_ref, commit_sha)?;

        let title = format!("chore(release): {}", prepared.tag);
        let notes = report.notes.clone().unwrap_or_default();
        let body = format!(
            "Automated release update for `{}`.\n\nThis PR updates the package version from `{}` to `{}`. Merging it into `{}` allows the release workflow to publish the tag and GitHub release.\n\n{}\n\n<!-- automated-release-branch: {} -->",
            prepared.tag,
            report.current_version,
            prepared.next_version,
            prepared.branch,
            notes,
            options.release_branch,
        );
        let pull_request = self.upsert_release_pull_request(
            owner,
            repo,
            &options.release_branch,
            &prepared.branch,
            &title,
            &body,
            report,
        )?;
        report.pull_request_number = Some(pull_request.number);
        report.pull_request_url = Some(pull_request.url);
        report.release_branch = Some(options.release_branch.clone());
        Ok(())
    }

    fn update_release_branch(
        &self,
        owner: &str,
        repo: &str,
        branch_ref: &str,
        commit_sha: &str,
    ) -> Result<()> {
        if self.github.reference_sha(owner, repo, branch_ref)?.is_some() {
            self.github.update_ref(owner, repo, branch_ref, commit_sha, true)
        } else {
            self.github.create_ref(owner, repo, branch_ref, commit_sha)
        }
    }

    fn upsert_release_pull_request(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        report: &mut ReleaseReport,
    ) -> Result<crate::github::PullRequestInfo> {
        let existing = self.github.find_open_pull_request(owner, repo, head, base)?;
        if let Some(existing) = existing {
            let updated =
                self.github.update_pull_request(owner, repo, existing.number, title, body)?;
            report.outcome = ReleaseOutcome::PullRequestUpdated;
            Ok(updated)
        } else {
            let created = self.github.create_pull_request(owner, repo, title, body, head, base)?;
            report.outcome = ReleaseOutcome::PullRequestCreated;
            Ok(created)
        }
    }

    fn build_commit(&self, options: &ReleaseOptions, prepared: &PreparedRelease) -> Result<String> {
        let owner = &options.owner;
        let repo = &options.repo;
        let repo_root = options.repo_root.canonicalize().with_context(|| {
            format!("failed to resolve repository root '{}'", options.repo_root.display())
        })?;

        let base_tree_sha = self.github.commit_tree_sha(owner, repo, &prepared.head_sha)?;
        let mut tree_entries = Vec::new();
        for update in &prepared.file_updates {
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
        let tag_target = match options.tag_style {
            TagStyle::Annotated => self.github.create_annotated_tag(
                owner,
                repo,
                &prepared.tag,
                &format!("Release {}", prepared.tag),
                commit_sha,
            )?,
            TagStyle::Lightweight => commit_sha.to_owned(),
        };
        self.github.create_ref(owner, repo, &format!("tags/{}", prepared.tag), &tag_target)?;

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
        pull_request_number: None,
        pull_request_url: None,
        release_branch: None,
        notes: None,
        commits_analyzed: analysis.commits.len(),
        commit_range_truncated: analysis.truncated,
        files_updated: Vec::new(),
    }
}

/// Read `extra_files` from the working tree so they ride along in the release
/// commit. Paths the version rewrite already covers are skipped: the rewrite
/// holds the bumped version, and staging a stale working-tree copy of the same
/// path would silently revert it.
fn extra_file_updates(
    repo_root: &Path,
    extra_files: &[PathBuf],
    planned: &[FileUpdate],
) -> Result<Vec<FileUpdate>> {
    if extra_files.is_empty() {
        return Ok(Vec::new());
    }

    let repo_root = repo_root
        .canonicalize()
        .with_context(|| format!("failed to resolve repository root '{}'", repo_root.display()))?;

    let mut updates = Vec::new();
    for extra_file in extra_files {
        let path =
            if extra_file.is_absolute() { extra_file.clone() } else { repo_root.join(extra_file) };
        let path = path.canonicalize().with_context(|| {
            format!("failed to resolve release file '{}'", extra_file.display())
        })?;
        // Keeps the commit inside the repository even when a caller passes an
        // absolute path or one containing `..`.
        remote::relative_repository_path(&repo_root, &path)?;

        if planned.iter().any(|update| update.file == path)
            || updates.iter().any(|update: &FileUpdate| update.file == path)
        {
            continue;
        }

        let updated_content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read release file '{}'", path.display()))?;
        updates.push(FileUpdate { file: path, updated_content });
    }

    Ok(updates)
}

fn validate_release_branch(branch: &str) -> Result<()> {
    if branch != AUTOMATED_RELEASE_BRANCH_PREFIX
        && !branch.starts_with(&format!("{AUTOMATED_RELEASE_BRANCH_PREFIX}/"))
    {
        anyhow::bail!(
            "automated release branch '{branch}' must use the reserved '{AUTOMATED_RELEASE_BRANCH_PREFIX}/' prefix"
        );
    }
    Ok(())
}

// Tests live in a sibling file to keep this module within the repository's
// file-size lint budget; they remain `super::`-scoped unit tests.
#[cfg(test)]
#[path = "release_tests.rs"]
#[allow(clippy::significant_drop_tightening)]
mod tests;
