//! CLI surface for the `release` subcommand: argument parsing, GitHub
//! Actions output plumbing, and result printing.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use github_actions_maintainer::{
    BumpLevel, GitHubClient, ReleaseAction, ReleaseOptions, ReleaseOutcome, ReleasePhase,
    ReleasePublisher, ReleaseReport, TagStyle,
};

use crate::resolve_repository;

#[derive(Debug, Args)]
pub struct ReleaseArgs {
    /// Repository root containing Cargo.toml.
    #[arg(long, env = "INPUT_REPO", default_value = ".")]
    pub repo: PathBuf,

    /// GitHub token used to create the release commit, tag, and release;
    /// falls back to `GITHUB_TOKEN`.
    #[arg(long, env = "INPUT_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Repository owner; falls back to `OWNER`, then `GITHUB_REPOSITORY`.
    #[arg(long, env = "INPUT_OWNER")]
    pub owner: Option<String>,

    /// Repository name; falls back to `REPO_NAME`, then `GITHUB_REPOSITORY`.
    #[arg(long = "repo-name", env = "INPUT_REPO-NAME")]
    pub repo_name: Option<String>,

    /// Branch to release from; defaults to the repository default branch.
    #[arg(long, env = "INPUT_BASE-BRANCH")]
    pub base_branch: Option<String>,

    /// Version bump strategy.
    #[arg(long, env = "INPUT_BUMP", value_enum, default_value_t = BumpArg::Auto)]
    pub bump: BumpArg,

    /// Prefix for release tags.
    #[arg(long, env = "INPUT_TAG-PREFIX", default_value = "v")]
    pub tag_prefix: String,

    /// How the release tag is created. Annotated tags carry a tagger identity
    /// and satisfy provenance checks; the major alias stays lightweight.
    #[arg(long, env = "INPUT_TAG-STYLE", value_enum, default_value_t = TagStyleArg::Annotated)]
    pub tag_style: TagStyleArg,

    /// Also move the moving major alias tag (e.g. v1) to the new release.
    #[arg(long, env = "INPUT_UPDATE-MAJOR-ALIAS", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    pub update_major_alias: bool,

    /// Path where generated release notes are written, including on dry runs.
    #[arg(long, env = "INPUT_NOTES-FILE", default_value = "release_notes.md")]
    pub notes_file: PathBuf,

    /// Commit message template; "{version}" is replaced with the new version.
    #[arg(long, env = "INPUT_COMMIT-MESSAGE", default_value = "chore: release v{version}")]
    pub commit_message: String,

    /// Create or update an automated release branch and pull request instead
    /// of publishing directly to the base branch.
    #[arg(long, env = "INPUT_CREATE-PR", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    pub create_pr: bool,

    /// Automation-owned branch used for the release pull request.
    #[arg(long, env = "INPUT_RELEASE-BRANCH", default_value = "automation/release")]
    pub release_branch: String,

    /// Extra repository-relative files to stage into the release commit, on
    /// top of the manifest and lockfile version rewrites. Lets a caller pin a
    /// value it can only resolve at release time, such as a runtime image
    /// digest, inside the commit the release tag points at.
    // No `default_value`: combined with `value_delimiter` an empty default
    // splits into zero values, and clap then rejects every `release` parse for
    // supplying no value.
    #[arg(long = "extra-files", env = "INPUT_EXTRA-FILES", value_delimiter = ',')]
    pub extra_files: Vec<PathBuf>,

    #[arg(long, env = "GITHUB_OUTPUT", hide = true)]
    pub github_output: Option<PathBuf>,

    /// Which part of the release to perform. `all` bumps, tags, and publishes
    /// in one run. `bump` stops after committing the version bump so a runtime
    /// image can be built from the released version before the tag exists, and
    /// `tag` then tags the version the manifest already holds.
    #[arg(long, env = "INPUT_PHASE", value_enum, default_value_t = PhaseArg::All)]
    pub phase: PhaseArg,

    /// Analyze and report without creating any commit, tag, or release.
    #[arg(long, env = "INPUT_DRY-RUN", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BumpArg {
    /// Derive the bump from conventional commits since the last release tag.
    Auto,
    Major,
    Minor,
    Patch,
}

impl BumpArg {
    const fn level(self) -> Option<BumpLevel> {
        match self {
            Self::Auto => None,
            Self::Major => Some(BumpLevel::Major),
            Self::Minor => Some(BumpLevel::Minor),
            Self::Patch => Some(BumpLevel::Patch),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TagStyleArg {
    /// Annotated tag object with a tagger identity (secure default).
    Annotated,
    /// Bare ref pointing directly at the release commit.
    Lightweight,
}

impl TagStyleArg {
    const fn style(self) -> TagStyle {
        match self {
            Self::Annotated => TagStyle::Annotated,
            Self::Lightweight => TagStyle::Lightweight,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PhaseArg {
    /// Bump, tag, and publish in a single run.
    All,
    /// Commit the version bump and stop, leaving the version untagged.
    Bump,
    /// Tag the version the manifest already holds, without bumping it.
    Tag,
}

impl PhaseArg {
    const fn phase(self) -> ReleasePhase {
        match self {
            Self::All => ReleasePhase::All,
            Self::Bump => ReleasePhase::Bump,
            Self::Tag => ReleasePhase::Tag,
        }
    }
}

pub fn run_release(args: ReleaseArgs, github_api_base_url: Option<String>) -> Result<()> {
    let github = GitHubClient::new(
        github_api_base_url.unwrap_or_else(|| String::from("https://api.github.com")),
        args.token,
    )?;
    let (owner, repo_name) = resolve_repository(args.owner.as_deref(), args.repo_name.as_deref())?;
    let publisher = ReleasePublisher::new(github);
    let report = publisher.release(&ReleaseOptions {
        repo_root: args.repo,
        owner,
        repo: repo_name,
        base_branch: args.base_branch,
        bump: args.bump.level(),
        tag_prefix: args.tag_prefix,
        tag_style: args.tag_style.style(),
        update_major_alias: args.update_major_alias,
        commit_message: args.commit_message,
        create_pr: args.create_pr,
        release_branch: args.release_branch,
        dry_run: args.dry_run,
        extra_files: args
            .extra_files
            .into_iter()
            .filter(|file| !file.as_os_str().is_empty())
            .collect(),
        phase: args.phase.phase(),
    })?;

    if let Some(notes) = &report.notes {
        fs::write(&args.notes_file, notes).with_context(|| {
            format!("failed to write release notes to '{}'", args.notes_file.display())
        })?;
    }

    if let Some(output_path) = &args.github_output {
        let outputs = report.github_outputs(&args.notes_file);
        let mut file =
            fs::OpenOptions::new().append(true).create(true).open(output_path).with_context(
                || format!("failed to open GitHub outputs file '{}'", output_path.display()),
            )?;
        file.write_all(outputs.as_bytes()).with_context(|| {
            format!("failed to write GitHub outputs to '{}'", output_path.display())
        })?;
    }

    print_release_report(&report, &args.notes_file);
    Ok(())
}

fn print_released(report: &ReleaseReport, notes_file: &Path) {
    println!(
        "Released {} ({} -> {}).",
        report.tag.as_deref().unwrap_or_default(),
        report.current_version,
        report.next_version.as_deref().unwrap_or_default()
    );
    if let Some(commit_sha) = &report.commit_sha {
        println!("- release commit {commit_sha}");
    }
    if let Some(major_alias) = &report.major_alias {
        println!("- moved major alias tag {major_alias}");
    }
    if let Some(release_url) = &report.release_url {
        println!("- release {release_url}");
    }
    for file in &report.files_updated {
        println!("- updated {}", file.display());
    }
    println!("- release notes written to {}", notes_file.display());
}

/// What a dry run would do: the headline's verb phrase and the next-action
/// sentence, which differ by mode even for the same version.
fn dry_run_plan(report: &ReleaseReport) -> (String, Option<String>) {
    let tag = report.tag.as_deref().unwrap_or_default();
    let next = report.next_version.as_deref().unwrap_or_default();
    match &report.action {
        Some(ReleaseAction::ProposeReleasePullRequest { release_branch, base }) => (
            format!("propose {tag} in the release pull request"),
            Some(format!(
                "point {release_branch} at a version-bump commit on top of {base} and open or update its pull request into {base}; nothing is tagged or released until it merges"
            )),
        ),
        Some(ReleaseAction::TagManifestVersion { branch }) => {
            let target = if report.files_updated.is_empty() {
                format!("tag the head of {branch} as {tag}")
            } else {
                format!("commit the staged files to {branch}, tag that commit as {tag},")
            };
            (
                format!("release {tag}"),
                Some(format!(
                    "{target} and publish its GitHub Release; the manifest already holds the untagged {next}, so nothing is bumped"
                )),
            )
        }
        Some(ReleaseAction::CommitBump { branch }) => (
            format!("commit version {next}"),
            Some(format!(
                "commit the version bump to {branch} and stop; {tag} is created by a later --phase tag run"
            )),
        ),
        Some(ReleaseAction::CommitAndRelease { branch }) => (
            format!("release {tag}"),
            Some(format!(
                "commit the version bump to {branch}, tag that commit as {tag}, and publish its GitHub Release"
            )),
        ),
        None => (format!("release {tag}"), None),
    }
}

/// Describe a dry run: the version it computed, the path the real run would
/// take with it, the files it would rewrite, and that nothing was written.
fn dry_run_lines(report: &ReleaseReport, notes_file: &Path) -> Vec<String> {
    let (would, next_action) = dry_run_plan(report);
    let mut lines = vec![format!(
        "Dry run: would {would} ({} -> {}).",
        report.current_version,
        report.next_version.as_deref().unwrap_or_default()
    )];
    if let Some(next_action) = next_action {
        lines.push(format!("- next action: {next_action}"));
    }
    lines.extend(
        report.files_updated.iter().map(|file| format!("- would update {}", file.display())),
    );
    lines.push(format!("- release notes written to {}", notes_file.display()));
    lines.push(String::from(
        "- nothing was written to the repository: no commit, branch, tag, pull request, or release",
    ));
    lines
}

/// Print the human-readable summary of a release run to the action log.
fn print_release_report(report: &ReleaseReport, notes_file: &Path) {
    match report.outcome {
        ReleaseOutcome::Released => print_released(report, notes_file),
        ReleaseOutcome::VersionCommitted => {
            println!(
                "Committed version {} ({} -> {}); tag and release are still pending.",
                report.next_version.as_deref().unwrap_or_default(),
                report.current_version,
                report.next_version.as_deref().unwrap_or_default()
            );
            if let Some(commit_sha) = &report.commit_sha {
                println!("- version commit {commit_sha}");
            }
            for file in &report.files_updated {
                println!("- updated {}", file.display());
            }
            println!("- publish the runtime image for this commit, then re-run with --phase tag");
        }
        ReleaseOutcome::PullRequestCreated | ReleaseOutcome::PullRequestUpdated => {
            let action = if report.outcome == ReleaseOutcome::PullRequestCreated {
                "Created"
            } else {
                "Updated"
            };
            println!(
                "{action} automated release PR for {} ({} -> {}).",
                report.tag.as_deref().unwrap_or_default(),
                report.current_version,
                report.next_version.as_deref().unwrap_or_default()
            );
            if let Some(branch) = &report.release_branch {
                println!("- release branch {branch}");
            }
            if let Some(url) = &report.pull_request_url {
                println!("- pull request {url}");
            }
            for file in &report.files_updated {
                println!("- updated {}", file.display());
            }
            println!("- release notes written to {}", notes_file.display());
        }
        ReleaseOutcome::DryRun => {
            for line in dry_run_lines(report, notes_file) {
                println!("{line}");
            }
        }
        ReleaseOutcome::SkippedNoReleasableChanges => {
            println!(
                "No release needed: {} commits since the last release contain no feat, fix, or breaking changes.",
                report.commits_analyzed
            );
        }
        ReleaseOutcome::SkippedRace => {
            println!(
                "Skipped release: the branch advanced past the analyzed commit. Re-run against the new head."
            );
        }
        ReleaseOutcome::SkippedTagExists => {
            println!(
                "Skipped release: tag {} already exists. If its GitHub Release is missing, create it manually.",
                report.tag.as_deref().unwrap_or_default()
            );
        }
    }

    if report.commit_range_truncated {
        println!(
            "- note: the analyzed commit list was truncated; release notes may be incomplete."
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use github_actions_maintainer::{ReleaseAction, ReleaseOutcome, ReleaseReport};

    use super::dry_run_lines;

    /// A dry-run report for `current -> next` taking `action`.
    fn dry_run_report(
        current: &str,
        next: &str,
        action: ReleaseAction,
        files: &[&str],
    ) -> ReleaseReport {
        ReleaseReport {
            outcome: ReleaseOutcome::DryRun,
            action: Some(action),
            current_version: current.to_owned(),
            next_version: Some(next.to_owned()),
            bump: None,
            tag: Some(format!("v{next}")),
            major_alias: None,
            commit_sha: None,
            release_url: None,
            pull_request_number: None,
            pull_request_url: None,
            release_branch: None,
            notes: Some(String::from("## Release")),
            commits_analyzed: 1,
            commit_range_truncated: false,
            files_updated: files.iter().map(PathBuf::from).collect(),
        }
    }

    /// The dry-run lines for `report`, with the default notes path.
    fn lines(report: &ReleaseReport) -> Vec<String> {
        dry_run_lines(report, Path::new("release_notes.md"))
    }

    const NOTHING_WRITTEN: &str =
        "- nothing was written to the repository: no commit, branch, tag, pull request, or release";

    /// Push mode commits, tags, and releases in the same run.
    #[test]
    fn push_mode_dry_run_names_the_commit_tag_and_release() {
        let report = dry_run_report(
            "0.2.3",
            "0.3.0",
            ReleaseAction::CommitAndRelease { branch: String::from("main") },
            &["Cargo.toml"],
        );

        assert_eq!(
            lines(&report),
            [
                "Dry run: would release v0.3.0 (0.2.3 -> 0.3.0).",
                "- next action: commit the version bump to main, tag that commit as v0.3.0, and publish its GitHub Release",
                "- would update Cargo.toml",
                "- release notes written to release_notes.md",
                NOTHING_WRITTEN,
            ]
        );
    }

    /// Release-pull-request mode proposes the bump instead of releasing it.
    #[test]
    fn create_pr_dry_run_names_the_release_pull_request_not_a_release() {
        let report = dry_run_report(
            "0.5.1",
            "0.5.2",
            ReleaseAction::ProposeReleasePullRequest {
                release_branch: String::from("automation/release"),
                base: String::from("main"),
            },
            &["Cargo.toml", "Cargo.lock"],
        );

        assert_eq!(
            lines(&report),
            [
                "Dry run: would propose v0.5.2 in the release pull request (0.5.1 -> 0.5.2).",
                "- next action: point automation/release at a version-bump commit on top of main and open or update its pull request into main; nothing is tagged or released until it merges",
                "- would update Cargo.toml",
                "- would update Cargo.lock",
                "- release notes written to release_notes.md",
                NOTHING_WRITTEN,
            ]
        );
    }

    /// After a merged release pull request the run tags without bumping.
    #[test]
    fn create_pr_dry_run_after_a_merged_release_pull_request_names_the_tag() {
        let report = dry_run_report(
            "0.5.1",
            "0.5.1",
            ReleaseAction::TagManifestVersion { branch: String::from("main") },
            &[],
        );

        assert_eq!(
            lines(&report),
            [
                "Dry run: would release v0.5.1 (0.5.1 -> 0.5.1).",
                "- next action: tag the head of main as v0.5.1 and publish its GitHub Release; the manifest already holds the untagged 0.5.1, so nothing is bumped",
                "- release notes written to release_notes.md",
                NOTHING_WRITTEN,
            ]
        );
    }

    /// Staged extra files are committed before the tag is created.
    #[test]
    fn tag_phase_dry_run_with_staged_files_commits_them_before_tagging() {
        let report = dry_run_report(
            "0.7.6",
            "0.7.6",
            ReleaseAction::TagManifestVersion { branch: String::from("main") },
            &["runtime/Dockerfile"],
        );

        assert_eq!(
            lines(&report)[1],
            "- next action: commit the staged files to main, tag that commit as v0.7.6, and publish its GitHub Release; the manifest already holds the untagged 0.7.6, so nothing is bumped"
        );
    }

    /// The bump phase commits and leaves the tag to the tag phase.
    #[test]
    fn bump_phase_dry_run_leaves_the_tag_to_the_next_phase() {
        let report = dry_run_report(
            "0.7.5",
            "0.7.6",
            ReleaseAction::CommitBump { branch: String::from("main") },
            &["Cargo.toml"],
        );

        assert_eq!(
            lines(&report)[..2],
            [
                "Dry run: would commit version 0.7.6 (0.7.5 -> 0.7.6).",
                "- next action: commit the version bump to main and stop; v0.7.6 is created by a later --phase tag run",
            ]
        );
    }
}
