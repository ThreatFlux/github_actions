//! Automated release publishing driven by conventional commits.
//!
//! The publisher analyzes commits since the last release tag through the
//! GitHub REST API, bumps the Cargo manifest version, and creates the release
//! commit, tag, and GitHub Release without shelling out to git â€” so it runs in
//! minimal containers. Failures after object creation but before the branch
//! ref update leave only unreachable git objects behind, which GitHub
//! garbage-collects; the window between tag creation and release creation is
//! not auto-healed and requires a manual `gh release create` for the tag.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use regex::Regex;
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

/// What a run does once its analysis finds a version to release, or on a dry
/// run what it would have done. Recorded so a dry run can say which path the
/// real run takes, not just which version it computed.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReleaseAction {
    /// Commit the version bump to `branch`, tag that commit, and publish the
    /// GitHub Release.
    CommitAndRelease { branch: String },
    /// Commit the version bump to `branch` and stop, leaving the tag to a later
    /// [`ReleasePhase::Tag`] run.
    CommitBump { branch: String },
    /// Tag the version the manifest already holds on `branch` and publish its
    /// GitHub Release, without bumping it. Only staged extra files, if any, are
    /// committed first.
    TagManifestVersion { branch: String },
    /// Point `release_branch` at a prepared release commit on top of `base` and
    /// open or update its pull request into `base`. Nothing is tagged until the
    /// pull request merges.
    ProposeReleasePullRequest { release_branch: String, base: String },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseReport {
    pub outcome: ReleaseOutcome,
    /// The path the run took (or, on a dry run, would take); `None` when it
    /// stopped before choosing one because nothing needed releasing or the tag
    /// already exists.
    pub action: Option<ReleaseAction>,
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
    preview_sources: Option<BTreeMap<String, String>>,
    current_version: String,
    current: Version,
    /// Name of the latest release tag and the version it carries, both absent
    /// until the first release.
    last_tag: Option<String>,
    last_released: Option<Version>,
    commits: Vec<ConventionalCommit>,
    truncated: bool,
}

impl Analysis {
    /// True when the manifest holds a version that was never tagged.
    ///
    /// A direct release tags the version in the same run that bumps it, so the
    /// two only diverge once a release pull request lands its bump on the base
    /// branch without a tag - or on a repository that has never released.
    fn manifest_unreleased(&self) -> bool {
        self.last_released.as_ref().is_none_or(|released| self.current > *released)
    }

    fn inferred_bump(&self, pre_v1_breaking_minor: bool) -> Option<BumpLevel> {
        let baseline = self.last_released.as_ref().unwrap_or(&self.current);
        conventional::required_bump(&self.commits).map(|bump| {
            if pre_v1_breaking_minor && baseline.major == 0 && bump == BumpLevel::Major {
                BumpLevel::Minor
            } else {
                bump
            }
        })
    }

    /// Recheck the merged bump against the entire release range before tagging.
    fn release_pr_correction(&self, pre_v1_breaking_minor: bool) -> Result<Option<BumpLevel>> {
        let (Some(tag), Some(previous)) = (&self.last_tag, &self.last_released) else {
            return Ok(None);
        };
        anyhow::ensure!(
            !self.truncated,
            "cannot tag version {} with an incomplete commit range since {tag}",
            self.current
        );
        Ok(self
            .inferred_bump(pre_v1_breaking_minor)
            .filter(|&bump| self.current < conventional::bump_version(previous, bump)))
    }

    /// Refuse to release from a manifest older than the latest release tag.
    ///
    /// The commit range starts at the highest tag, but the next version is
    /// bumped from the manifest. A manifest behind that tag therefore yields a
    /// version at or below one already released: it either collides with an
    /// existing tag or publishes a release older than the latest one, and as
    /// the range never moves past the highest tag, every later run repeats it.
    fn ensure_manifest_not_behind_latest_tag(&self) -> Result<()> {
        match (&self.last_tag, &self.last_released) {
            (Some(tag), Some(released)) if self.current < *released => bail!(
                "Cargo.toml version {current} is lower than the latest release tag {tag}. \
                 Releasing would bump from {current} and produce a version at or below one that is \
                 already tagged. Set the manifest version to {released}, or delete {tag} if it was \
                 created by mistake, then re-run.",
                current = self.current_version,
            ),
            _ => Ok(()),
        }
    }
}

#[derive(Debug)]
struct PreparedRelease {
    branch: String,
    head_sha: String,
    next_version: Version,
    tag: String,
    file_updates: Vec<FileUpdate>,
    action: ReleaseAction,
    release_branch_head: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReleasePublisher {
    github: GitHubClient,
    draft_release: bool,
    pre_v1_breaking_minor: bool,
}

impl ReleasePublisher {
    #[must_use]
    pub const fn new(github: GitHubClient) -> Self {
        Self { github, draft_release: false, pre_v1_breaking_minor: false }
    }

    /// Create a draft release object so a native publisher can attach and
    /// verify its complete asset inventory before explicitly publishing it.
    /// This changes no tag, source, phase, or pull-request guard.
    #[must_use]
    pub const fn with_draft_release(mut self, draft_release: bool) -> Self {
        self.draft_release = draft_release;
        self
    }

    /// Infer a minor release for breaking changes to a 0.x package.
    #[must_use]
    pub const fn with_pre_v1_breaking_minor(mut self, enabled: bool) -> Self {
        self.pre_v1_breaking_minor = enabled;
        self
    }

    /// Analyze the commits since the latest release tag and release, propose,
    /// or (on a dry run) report the next version according to `options`.
    pub fn release(&self, options: &ReleaseOptions) -> Result<ReleaseReport> {
        self.release_with_expected_head(options, None, None)
    }

    /// Release only if the branch still points at the exact commit validated
    /// by the caller. Reject a mismatch before reading release history or
    /// preparing any writes; the existing mid-run race checks still apply.
    pub fn release_at_head(
        &self,
        options: &ReleaseOptions,
        expected_head: &str,
    ) -> Result<ReleaseReport> {
        anyhow::ensure!(
            !expected_head.trim().is_empty(),
            "expected release head must not be empty"
        );
        self.release_with_expected_head(options, Some(expected_head), None)
    }

    /// Preview an immutable candidate commit instead of the current base head.
    /// Every manifest and lockfile consulted must match that commit. This API
    /// cannot publish or relax the validated-head guard used by real releases.
    pub fn preview_at_commit(
        &self,
        options: &ReleaseOptions,
        analysis_sha: &str,
    ) -> Result<ReleaseReport> {
        anyhow::ensure!(options.dry_run, "analysis SHA is only supported for dry-run previews");
        anyhow::ensure!(
            analysis_sha.len() == 40 && analysis_sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "analysis SHA must be a full immutable 40-character commit SHA"
        );
        self.release_with_expected_head(options, None, Some(analysis_sha))
    }

    fn release_with_expected_head(
        &self,
        options: &ReleaseOptions,
        expected_head: Option<&str>,
        analysis_sha: Option<&str>,
    ) -> Result<ReleaseReport> {
        self.github.ensure_token()?;
        if options.create_pr {
            validate_release_branch(&options.release_branch)?;
        }
        let analysis = self.analyze(options, expected_head, analysis_sha)?;
        analysis.ensure_manifest_not_behind_latest_tag()?;
        let tag_merged_release_pr = options.create_pr && analysis.manifest_unreleased();
        let correction = if tag_merged_release_pr {
            analysis.release_pr_correction(self.pre_v1_breaking_minor)?
        } else {
            None
        };
        if tag_merged_release_pr && options.phase == ReleasePhase::Bump {
            anyhow::ensure!(
                correction.is_some(),
                "the manifest already holds an untagged version; complete it before proposing another bump"
            );
        }
        if options.phase == ReleasePhase::Tag {
            anyhow::ensure!(
                correction.is_none(),
                "the pending release version is below its release range; request phase bump to propose the correction"
            );
        }
        let report = if options.phase == ReleasePhase::Tag
            || (options.phase == ReleasePhase::All && tag_merged_release_pr && correction.is_none())
        {
            self.release_manifest_version(options, analysis)?
        } else {
            self.release_bumped_version(options, analysis, correction)?
        };
        anyhow::ensure!(
            expected_head.is_none() || report.outcome != ReleaseOutcome::SkippedRace,
            "release branch advanced after validating {}; the requested release was not performed",
            expected_head.unwrap_or_default()
        );
        Ok(report)
    }

    /// Bump the manifest version from the conventional commits, then commit it.
    /// [`ReleasePhase::Bump`] stops there; otherwise the same run tags it.
    fn release_bumped_version(
        &self,
        options: &ReleaseOptions,
        analysis: Analysis,
        correction: Option<BumpLevel>,
    ) -> Result<ReleaseReport> {
        let bump = options.bump.or_else(|| analysis.inferred_bump(self.pre_v1_breaking_minor));
        let bump = bump.max(correction);
        let mut report = initial_report(&analysis, bump);

        let Some(bump_level) = bump else {
            return Ok(report);
        };

        let baseline = if correction.is_some() {
            analysis.last_released.as_ref().context("release correction requires a previous tag")?
        } else {
            &analysis.current
        };
        let release_branch_head = if options.create_pr {
            self.github.reference_sha(
                &options.owner,
                &options.repo,
                &format!("heads/{}", options.release_branch),
            )?
        } else {
            None
        };
        let mut next_version = conventional::bump_version(baseline, bump_level);
        if options.bump.is_none()
            && let Some(head) = release_branch_head.as_deref()
            && let Some(pending) = self.pending_release_version(options, &analysis.branch, head)?
        {
            next_version = next_version.max(pending);
        }
        let next = next_version.to_string();
        let tag = format!("{}{next}", options.tag_prefix);
        report.notes = Some(release_notes(options, &tag, &analysis));
        report.next_version = Some(next);
        report.tag = Some(tag.clone());

        if self.tag_exists(options, &tag)? {
            report.outcome = ReleaseOutcome::SkippedTagExists;
            return Ok(report);
        }

        let plan = self.version_rewrite_plan(options, &analysis, &next_version.to_string())?;
        let mut file_updates = plan.file_updates;
        let extra = extra_file_updates(&options.repo_root, &options.extra_files, &file_updates)?;
        file_updates.extend(extra);

        let action = if options.create_pr {
            ReleaseAction::ProposeReleasePullRequest {
                release_branch: options.release_branch.clone(),
                base: analysis.branch.clone(),
            }
        } else if options.phase == ReleasePhase::Bump {
            ReleaseAction::CommitBump { branch: analysis.branch.clone() }
        } else {
            ReleaseAction::CommitAndRelease { branch: analysis.branch.clone() }
        };
        self.prepare_and_publish(
            options,
            &PreparedRelease {
                branch: analysis.branch,
                head_sha: analysis.head_sha,
                next_version,
                tag,
                file_updates,
                action,
                release_branch_head,
            },
            report,
        )
    }

    fn pending_release_version(
        &self,
        options: &ReleaseOptions,
        base: &str,
        head: &str,
    ) -> Result<Option<Version>> {
        let owner = &options.owner;
        let repo = &options.repo;
        if self.github.find_open_pull_request(owner, repo, &options.release_branch, base)?.is_none()
        {
            return Ok(None);
        }
        let tracked = self.github.regular_files_at_commit(owner, repo, head)?;
        let blob = tracked.get("Cargo.toml").context("open release proposal has no Cargo.toml")?;
        let raw = self.github.release_source_blob(owner, repo, "Cargo.toml", blob)?;
        let version = versioning::current_version_from_source(
            std::str::from_utf8(raw.as_ref())
                .context("open release proposal manifest is not UTF-8")?,
        )?;
        Ok(Some(Version::parse(&version).context("open release proposal has an invalid version")?))
    }

    fn version_rewrite_plan(
        &self,
        options: &ReleaseOptions,
        analysis: &Analysis,
        next_version: &str,
    ) -> Result<versioning::VersionRewritePlan> {
        // Legacy manual dry runs can still inspect local data. An explicit
        // candidate preview uses the same immutable-source checks as a writer.
        if options.dry_run && analysis.preview_sources.is_none() {
            return versioning::plan_read_only_rewrite(&options.repo_root, next_version);
        }
        let repo_root = options.repo_root.canonicalize()?;
        let tracked = match &analysis.preview_sources {
            Some(tracked) => tracked.clone(),
            None => self.github.regular_files_at_commit(
                &options.owner,
                &options.repo,
                &analysis.head_sha,
            )?,
        };
        if !options.dry_run
            && versioning::tracked_nested_lockfiles(&tracked.keys().cloned().collect())
                .next()
                .is_none()
        {
            return versioning::plan_version_rewrite(&options.repo_root, next_version);
        }
        let rewrite = versioning::plan_version_rewrite_in_tree(
            &repo_root,
            next_version,
            &tracked.keys().cloned().collect(),
        )?;
        self.github.validate_release_sources(
            &options.owner,
            &options.repo,
            &repo_root,
            &tracked,
            &rewrite.original_files,
        )?;
        Ok(rewrite.plan)
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
        report.notes = Some(release_notes(options, &tag, &analysis));
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

        let propose_files =
            options.create_pr && options.phase == ReleasePhase::Tag && !file_updates.is_empty();
        let (action, release_branch_head) = if propose_files {
            (
                ReleaseAction::ProposeReleasePullRequest {
                    release_branch: options.release_branch.clone(),
                    base: analysis.branch.clone(),
                },
                self.github.reference_sha(
                    &options.owner,
                    &options.repo,
                    &format!("heads/{}", options.release_branch),
                )?,
            )
        } else {
            (ReleaseAction::TagManifestVersion { branch: analysis.branch.clone() }, None)
        };
        self.prepare_and_publish(
            options,
            &PreparedRelease {
                branch: analysis.branch,
                head_sha: analysis.head_sha,
                next_version: version,
                tag,
                file_updates,
                action,
                release_branch_head,
            },
            report,
        )
    }

    /// Record the files and the action the run takes, then stop for a dry run
    /// or publish the prepared release.
    fn prepare_and_publish(
        &self,
        options: &ReleaseOptions,
        prepared: &PreparedRelease,
        mut report: ReleaseReport,
    ) -> Result<ReleaseReport> {
        report.files_updated =
            prepared.file_updates.iter().map(|update| update.file.clone()).collect();
        report.action = Some(prepared.action.clone());

        if options.dry_run {
            report.outcome = ReleaseOutcome::DryRun;
            return Ok(report);
        }

        self.publish(options, prepared, &mut report)?;
        Ok(report)
    }

    fn tag_exists(&self, options: &ReleaseOptions, tag: &str) -> Result<bool> {
        Ok(self
            .github
            .reference_sha(&options.owner, &options.repo, &format!("tags/{tag}"))?
            .is_some())
    }

    /// Read the branch head, the manifest version, the latest release tag, and
    /// the commits since that tag (or the recent history before the first
    /// release).
    fn analyze(
        &self,
        options: &ReleaseOptions,
        expected_head: Option<&str>,
        analysis_sha: Option<&str>,
    ) -> Result<Analysis> {
        let owner = &options.owner;
        let repo = &options.repo;
        let (branch, head_sha) = self.analysis_source(options, expected_head, analysis_sha)?;
        let (preview_sources, current_version) =
            self.analysis_manifest(options, &head_sha, analysis_sha.is_some())?;
        let current = Version::parse(&current_version).with_context(|| {
            format!("invalid current version '{current_version}' in Cargo.toml")
        })?;

        let last_tag = self.github.latest_semver_tag(owner, repo, &options.tag_prefix)?;
        let last_released = last_tag
            .as_ref()
            .and_then(|tag| tag.name.strip_prefix(options.tag_prefix.as_str()))
            .and_then(|version| Version::parse(version).ok());
        let range = match &last_tag {
            Some(tag) => self.github.compare_commit_history(
                owner,
                repo,
                &tag.name,
                &head_sha,
                MAX_COMPARE_PAGES,
            )?,
            None => {
                self.github.list_commit_history(owner, repo, &head_sha, MAX_FIRST_RELEASE_PAGES)?
            }
        };
        let truncated = range.truncated;
        let commits = if options.create_pr {
            self.classify_reviewed_commits(owner, repo, &range.commits)?
        } else {
            let range = crate::github::CommitRange::from(range);
            conventional::classify_commits(&range.commits)
        };

        Ok(Analysis {
            branch,
            head_sha,
            preview_sources,
            current_version,
            current,
            last_tag: last_tag.map(|tag| tag.name),
            last_released,
            commits,
            truncated,
        })
    }

    fn classify_reviewed_commits(
        &self,
        owner: &str,
        repo: &str,
        commits: &[crate::github::HistoryCommit],
    ) -> Result<Vec<ConventionalCommit>> {
        // Read the setting only for a wrapped breaking subject. GitHub can
        // omit it for read-only tokens, so in that case corroborate the
        // body line against the actual merged pull request title.
        let mut merge_setting = None;
        conventional::classify_reviewed_history(commits, |sha, number, title| {
            if merge_setting.is_none() {
                merge_setting = Some(self.github.merge_message_uses_pr_title(owner, repo)?);
            }
            let uses_pr_title = merge_setting.flatten();
            if uses_pr_title == Some(false) {
                return Ok(false);
            }
            let expected_title = uses_pr_title.is_none().then_some(title);
            self.github.merged_pull_request_matches(owner, repo, number, sha, expected_title)
        })
    }

    /// Resolve history independently from manifest loading so both keep one
    /// explicit immutable source, while writers retain the current-head guard.
    fn analysis_source(
        &self,
        options: &ReleaseOptions,
        expected_head: Option<&str>,
        analysis_sha: Option<&str>,
    ) -> Result<(String, String)> {
        let branch = match options.base_branch.as_deref().map(str::trim) {
            Some(branch) if !branch.is_empty() => branch.to_owned(),
            _ => self.github.default_branch(&options.owner, &options.repo)?,
        };
        let head_sha = match analysis_sha {
            Some(sha) => sha.to_owned(),
            None => self.github.branch_head_sha(&options.owner, &options.repo, &branch)?,
        };
        if let Some(expected) = expected_head {
            anyhow::ensure!(
                head_sha == expected,
                "release branch '{branch}' head {head_sha} does not match expected head {expected}; validate the current head before releasing"
            );
        }
        Ok((branch, head_sha))
    }

    /// Authenticate and parse the same root-manifest snapshot for previews.
    fn analysis_manifest(
        &self,
        options: &ReleaseOptions,
        head_sha: &str,
        immutable_preview: bool,
    ) -> Result<(Option<BTreeMap<String, String>>, String)> {
        if !immutable_preview {
            return Ok((None, versioning::current_version(&options.repo_root)?));
        }
        let tracked =
            self.github.regular_files_at_commit(&options.owner, &options.repo, head_sha)?;
        let root = options.repo_root.canonicalize()?;
        let manifest = root.join("Cargo.toml");
        let contents = fs::read_to_string(&manifest)?;
        let originals = BTreeMap::from([(manifest, contents.clone())]);
        self.github.validate_release_sources(
            &options.owner,
            &options.repo,
            &root,
            &tracked,
            &originals,
        )?;
        Ok((Some(tracked), versioning::current_version_from_source(&contents)?))
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

        if matches!(prepared.action, ReleaseAction::ProposeReleasePullRequest { .. }) {
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
        if !self.update_release_branch(
            owner,
            repo,
            &branch_ref,
            prepared.release_branch_head.as_deref(),
            commit_sha,
        )? {
            report.outcome = ReleaseOutcome::SkippedRace;
            return Ok(());
        }

        let (title, change) = if options.phase == ReleasePhase::Tag {
            (
                format!("chore(release): prepare artifacts for {}", prepared.tag),
                format!(
                    "This PR records the release files for `{}`. After review and merge into `{}`, the release workflow revalidates the resulting commit before publishing.",
                    prepared.tag, prepared.branch
                ),
            )
        } else {
            (
                format!("chore(release): {}", prepared.tag),
                format!(
                    "This PR updates the package version from `{}` to `{}`. Merging it into `{}` allows the release workflow to publish the tag and GitHub release.",
                    report.current_version, prepared.next_version, prepared.branch
                ),
            )
        };
        let notes = report.notes.clone().unwrap_or_default();
        let body = if options.phase == ReleasePhase::All {
            format!(
                "Automated release update for `{}`.\n\n{change}\n\n{notes}\n\n<!-- automated-release-branch: {} -->",
                prepared.tag, options.release_branch,
            )
        } else {
            format!(
                "## Summary\n\n{change}\n\n## Validation\n\n- Proposal base: `{}`. Its GitHub checks and this PR's checks are reviewed separately.\n- `make ci` was not run locally.\n\n## Checklist\n\n- [ ] Review the proposed release files and their GitHub checks.\n- [ ] Review the hosted Security and Docker results.\n- [x] No documentation or template bootstrap changes are needed for this release proposal.\n\n{notes}\n\n<!-- automated-release-branch: {} -->",
                prepared.head_sha, options.release_branch,
            )
        };
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
        previous_head: Option<&str>,
        commit_sha: &str,
    ) -> Result<bool> {
        if previous_head.is_some() {
            return self.github.update_ref_fast_forward(owner, repo, branch_ref, commit_sha);
        }
        match self.github.create_ref(owner, repo, branch_ref, commit_sha) {
            Ok(()) => Ok(true),
            Err(error) => {
                if self.github.reference_sha(owner, repo, branch_ref)?.is_some() {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
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
        let mut parents = vec![prepared.head_sha.as_str()];
        // The tree comes from the current base, but retaining the old proposal as
        // a parent makes refreshing the automation ref a fast-forward. A competing
        // refresh based on the same old proposal cannot replace a newer commit.
        if let Some(previous) = prepared.release_branch_head.as_deref()
            && previous != prepared.head_sha
        {
            parents.push(previous);
        }
        self.github.create_commit_with_parents(owner, repo, &message, &tree_sha, parents)
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
        let name = format!("Release {}", prepared.tag);
        let release = if self.draft_release {
            self.github.create_draft_release(
                owner,
                repo,
                &prepared.tag,
                &name,
                &notes,
                commit_sha,
            )?
        } else {
            self.github.create_release(owner, repo, &prepared.tag, &name, &notes, commit_sha)?
        };
        report.release_url = Some(release.url);
        report.outcome = ReleaseOutcome::Released;
        Ok(())
    }
}

/// Release notes for `tag`, without the automation's own version-bump commits.
fn release_notes(options: &ReleaseOptions, tag: &str, analysis: &Analysis) -> String {
    let commits = without_release_commits(&analysis.commits, &options.commit_message);
    conventional::release_notes(tag, &commits, analysis.truncated)
}

/// Drop the commits that are this tool's own version bumps: the first line of
/// `commit_message` rendered for any version, also as squash-merged from a
/// release pull request (`chore: release v1.2.3 (#42)`). The tag phase and a
/// merged release pull request both release a range that contains the bump
/// commit, and listing it under "Maintenance" would only repeat the title.
fn without_release_commits(
    commits: &[ConventionalCommit],
    commit_message: &str,
) -> Vec<ConventionalCommit> {
    let template = commit_message.lines().next().unwrap_or_default().trim();
    let pattern = template
        .split("{version}")
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?");
    let Ok(release_commit) = Regex::new(&format!(r"^{pattern}(?: \(#\d+\))?$")) else {
        return commits.to_vec();
    };
    commits.iter().filter(|commit| !release_commit.is_match(&commit.subject)).cloned().collect()
}

/// A report that releases nothing yet; callers fill in what the run decides.
fn initial_report(analysis: &Analysis, bump: Option<BumpLevel>) -> ReleaseReport {
    ReleaseReport {
        outcome: ReleaseOutcome::SkippedNoReleasableChanges,
        action: None,
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
