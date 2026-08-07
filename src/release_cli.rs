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
    BumpLevel, GitHubClient, ReleaseOptions, ReleaseOutcome, ReleasePublisher, ReleaseReport,
};

use crate::resolve_repository;

#[derive(Debug, Args)]
pub struct ReleaseArgs {
    /// Repository root containing Cargo.toml.
    #[arg(long, default_value = ".")]
    repo: PathBuf,

    /// GitHub token used to create the release commit, tag, and release.
    #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Repository owner; falls back to `GITHUB_REPOSITORY`.
    #[arg(long, env = "OWNER")]
    owner: Option<String>,

    /// Repository name; falls back to `GITHUB_REPOSITORY`.
    #[arg(long = "repo-name", env = "REPO_NAME")]
    repo_name: Option<String>,

    /// Branch to release from; defaults to the repository default branch.
    #[arg(long)]
    base_branch: Option<String>,

    /// Version bump strategy.
    #[arg(long, value_enum, default_value_t = BumpArg::Auto)]
    bump: BumpArg,

    /// Prefix for release tags.
    #[arg(long, default_value = "v")]
    tag_prefix: String,

    /// Also move the moving major alias tag (e.g. v1) to the new release.
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    update_major_alias: bool,

    /// Path where generated release notes are written, including on dry runs.
    #[arg(long, default_value = "release_notes.md")]
    notes_file: PathBuf,

    /// Commit message template; "{version}" is replaced with the new version.
    #[arg(long, default_value = "chore: release v{version}")]
    commit_message: String,

    #[arg(long, env = "GITHUB_OUTPUT", hide = true)]
    github_output: Option<PathBuf>,

    /// Analyze and report without creating any commit, tag, or release.
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BumpArg {
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
        update_major_alias: args.update_major_alias,
        commit_message: args.commit_message,
        dry_run: args.dry_run,
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

fn print_release_report(report: &ReleaseReport, notes_file: &Path) {
    match report.outcome {
        ReleaseOutcome::Released => print_released(report, notes_file),
        ReleaseOutcome::DryRun => {
            println!(
                "Dry run: would release {} ({} -> {}).",
                report.tag.as_deref().unwrap_or_default(),
                report.current_version,
                report.next_version.as_deref().unwrap_or_default()
            );
            for file in &report.files_updated {
                println!("- would update {}", file.display());
            }
            println!("- release notes written to {}", notes_file.display());
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
