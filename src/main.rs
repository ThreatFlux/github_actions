use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use github_actions_maintainer::{
    BumpLevel, CargoUpdateOptions, CargoUpdater, CratesIoClient, FileUpdate, GitHubClient, PinMode,
    PinOptions, PullRequestOptions, ReleaseOptions, ReleaseOutcome, ReleasePublisher,
    ReleaseReport, RemoteUpdatePublisher, UpdateChange, UpdateMode, UpdateOptions, WorkflowPinner,
    WorkflowUpdater,
};

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "General-purpose GitHub Actions maintenance with secure pinning as the first feature."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(long, env = "GITHUB_API_BASE_URL", hide = true, global = true)]
    github_api_base_url: Option<String>,

    #[arg(long, env = "CRATES_IO_API_BASE_URL", hide = true, global = true)]
    crates_api_base_url: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Resolve floating GitHub Action refs to immutable commit SHAs.
    Pin(PinArgs),
    /// Update GitHub Actions to the latest release or tag, then pin them.
    Update(UpdateArgs),
    /// Report current and latest versions for GitHub Actions in workflows.
    Status(StatusArgs),
    /// Bump the Cargo version from conventional commits, then commit, tag, and
    /// publish a GitHub Release.
    Release(ReleaseArgs),
}

#[derive(Debug, Args)]
struct RepoArgs {
    /// Repository root to scan.
    #[arg(long, default_value = ".")]
    repo: PathBuf,

    /// Relative path to workflow files beneath the repository root.
    #[arg(long, default_value = ".github/workflows")]
    workflows_path: PathBuf,

    /// GitHub token used to raise API rate limits.
    #[arg(long, env = "GITHUB_TOKEN")]
    token: Option<String>,

    /// Repository owner used for remote PR creation.
    #[arg(long, env = "OWNER")]
    owner: Option<String>,

    /// Repository name used for remote PR creation.
    #[arg(long = "repo-name", env = "REPO_NAME")]
    repo_name: Option<String>,

    /// Create a branch and pull request remotely instead of rewriting files locally.
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    create_pr: bool,

    /// Override the base branch for remote PR creation.
    #[arg(long)]
    base_branch: Option<String>,

    /// Override the update branch name for remote PR creation.
    #[arg(long)]
    branch_name: Option<String>,

    /// Labels to add to a created pull request, comma-separated.
    #[arg(long, default_value = "dependencies")]
    labels: String,

    /// Pull request title for remote update mode.
    #[arg(long, default_value = "Update dependencies")]
    title: String,

    /// Commit message for remote update mode.
    #[arg(long, default_value = "Update dependencies")]
    commit_message: String,
}

#[derive(Debug, Args)]
struct PinArgs {
    #[command(flatten)]
    repo: RepoArgs,

    #[command(flatten)]
    #[allow(dead_code)]
    targets: TargetArgs,

    /// Show changes without rewriting files.
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    #[command(flatten)]
    repo: RepoArgs,

    #[command(flatten)]
    targets: TargetArgs,

    /// Show available updates without rewriting files.
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[command(flatten)]
    repo: RepoArgs,

    #[command(flatten)]
    targets: TargetArgs,

    #[arg(long, hide = true, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    _dry_run: bool,
}

#[derive(Debug, Args)]
struct ReleaseArgs {
    /// Repository root containing Cargo.toml.
    #[arg(long, default_value = ".")]
    repo: PathBuf,

    /// GitHub token used to create the release commit, tag, and release.
    #[arg(long, env = "GITHUB_TOKEN")]
    token: Option<String>,

    /// Repository owner; falls back to GITHUB_REPOSITORY.
    #[arg(long, env = "OWNER")]
    owner: Option<String>,

    /// Repository name; falls back to GITHUB_REPOSITORY.
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

#[derive(Debug, Args, Clone, Default)]
struct TargetArgs {
    /// Include GitHub Actions workflow updates.
    #[arg(long = "github-actions", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    github_actions: bool,

    /// Include cargo package dependency updates.
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    cargo: bool,

    /// Include both GitHub Actions and cargo package updates.
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    all: bool,
}

#[derive(Debug, Clone, Copy)]
struct SelectedTargets {
    github_actions: bool,
    cargo: bool,
}

impl TargetArgs {
    const fn resolve(&self) -> SelectedTargets {
        if self.all {
            return SelectedTargets { github_actions: true, cargo: true };
        }

        if !self.github_actions && !self.cargo {
            return SelectedTargets { github_actions: true, cargo: false };
        }

        SelectedTargets { github_actions: self.github_actions, cargo: self.cargo }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Pin(args) => run_pin(args, cli.github_api_base_url),
        Commands::Update(args) => {
            run_update(args, cli.github_api_base_url, cli.crates_api_base_url)
        }
        Commands::Status(args) => {
            run_status(args, cli.github_api_base_url, cli.crates_api_base_url)
        }
        Commands::Release(args) => run_release(args, cli.github_api_base_url),
    }
}

fn run_release(args: ReleaseArgs, github_api_base_url: Option<String>) -> Result<()> {
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

fn print_release_report(report: &ReleaseReport, notes_file: &Path) {
    match report.outcome {
        ReleaseOutcome::Released => {
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

fn run_pin(args: PinArgs, github_api_base_url: Option<String>) -> Result<()> {
    let github = GitHubClient::new(
        github_api_base_url.unwrap_or_else(|| String::from("https://api.github.com")),
        args.repo.token,
    )?;
    let pinner = WorkflowPinner::new(github);
    let report = pinner.pin(&PinOptions {
        repo_root: args.repo.repo,
        workflows_path: args.repo.workflows_path,
        mode: if args.dry_run { PinMode::DryRun } else { PinMode::Apply },
    })?;

    if report.changes.is_empty() {
        println!(
            "No floating GitHub Actions references needed pinning. Scanned {} references across {} workflow files; {} were already pinned.",
            report.references_scanned, report.workflow_files, report.already_pinned
        );
        return Ok(());
    }

    if args.dry_run {
        println!(
            "Dry run: would pin {} action references across {} files.",
            report.changes.len(),
            report.changed_files()
        );
    } else {
        println!(
            "Pinned {} action references across {} files.",
            report.changes.len(),
            report.changed_files()
        );
    }

    for change in &report.changes {
        println!(
            "- {}:{} {}@{} -> {}",
            change.file.display(),
            change.line_number,
            change.action_slug,
            change.from_version,
            change.to_sha
        );
    }

    Ok(())
}

fn run_update(
    args: UpdateArgs,
    github_api_base_url: Option<String>,
    crates_api_base_url: Option<String>,
) -> Result<()> {
    let repo_args = args.repo;
    let repo_root = repo_args.repo.clone();
    let workflows_path = repo_args.workflows_path.clone();
    let token = repo_args.token.clone();
    let remote_mode = repo_args.create_pr;
    let targets = args.targets.resolve();
    let update_mode =
        if args.dry_run || remote_mode { UpdateMode::DryRun } else { UpdateMode::Apply };

    let action_report = if targets.github_actions {
        let github = GitHubClient::new(
            github_api_base_url.clone().unwrap_or_else(|| String::from("https://api.github.com")),
            token.clone(),
        )?;
        let updater = WorkflowUpdater::new(github);
        Some(updater.update(&UpdateOptions {
            repo_root: repo_root.clone(),
            workflows_path,
            mode: update_mode,
        })?)
    } else {
        None
    };

    let cargo_report = if targets.cargo {
        let crates_io = CratesIoClient::new(
            crates_api_base_url.unwrap_or_else(|| String::from("https://crates.io/api/v1")),
        )?;
        let updater = CargoUpdater::new(crates_io);
        Some(
            updater
                .update(&CargoUpdateOptions { repo_root: repo_root.clone(), mode: update_mode })?,
        )
    } else {
        None
    };

    let mut combined_changes = Vec::new();
    let mut combined_file_updates = Vec::new();
    if let Some(report) = &action_report {
        combined_changes.extend(report.changes.clone());
        combined_file_updates.extend(report.file_updates.clone());
    }
    if let Some(report) = &cargo_report {
        combined_changes.extend(report.changes.clone());
        combined_file_updates.extend(report.file_updates.clone());
    }

    if combined_changes.is_empty() {
        print_update_noop_summary(action_report.as_ref(), cargo_report.as_ref());
        return Ok(());
    }

    if remote_mode {
        if args.dry_run {
            println!(
                "Would create a pull request with {} dependency updates across {} files.",
                combined_changes.len(),
                count_changed_files(&combined_file_updates)
            );
        } else {
            let github = GitHubClient::new(
                github_api_base_url.unwrap_or_else(|| String::from("https://api.github.com")),
                token,
            )?;
            let (owner, repo_name) = resolve_remote_repository(&repo_args)?;
            let publisher = RemoteUpdatePublisher::new(github);
            let result = publisher
                .publish(
                    &combined_file_updates,
                    &combined_changes,
                    &PullRequestOptions {
                        repo_root,
                        owner,
                        repo: repo_name,
                        base_branch: repo_args.base_branch,
                        branch_name: repo_args.branch_name,
                        labels: parse_labels(&repo_args.labels),
                        title: repo_args.title,
                        commit_message: repo_args.commit_message,
                    },
                )?
                .expect("remote publish returns a pull request result when changes exist");

            println!(
                "Created pull request #{} on branch {}: {}",
                result.number, result.branch_name, result.url
            );
        }

        print_update_changes(&combined_changes);

        return Ok(());
    }

    println!(
        "{} {} dependency updates across {} files.",
        if args.dry_run { "Would apply" } else { "Applied" },
        combined_changes.len(),
        count_changed_files(&combined_file_updates)
    );

    print_update_changes(&combined_changes);

    Ok(())
}

fn parse_labels(raw: &str) -> Vec<String> {
    raw.split(',').map(str::trim).filter(|label| !label.is_empty()).map(ToOwned::to_owned).collect()
}

fn resolve_remote_repository(args: &RepoArgs) -> Result<(String, String)> {
    resolve_repository(args.owner.as_deref(), args.repo_name.as_deref())
}

fn resolve_repository(owner: Option<&str>, repo_name: Option<&str>) -> Result<(String, String)> {
    match (owner.map(str::trim), repo_name.map(str::trim)) {
        (Some(owner), Some(repo_name)) if !owner.is_empty() && !repo_name.is_empty() => {
            return Ok((owner.to_owned(), repo_name.to_owned()));
        }
        _ => {}
    }

    if let Ok(repository) = std::env::var("GITHUB_REPOSITORY")
        && let Some((owner, repo_name)) = repository.split_once('/')
    {
        return Ok((owner.to_owned(), repo_name.to_owned()));
    }

    anyhow::bail!(
        "--owner and --repo-name (or the GITHUB_REPOSITORY environment variable) are required"
    )
}

fn run_status(
    args: StatusArgs,
    github_api_base_url: Option<String>,
    crates_api_base_url: Option<String>,
) -> Result<()> {
    let targets = args.targets.resolve();
    let printed_section = if targets.github_actions {
        let github = GitHubClient::new(
            github_api_base_url.unwrap_or_else(|| String::from("https://api.github.com")),
            args.repo.token.clone(),
        )?;
        let updater = WorkflowUpdater::new(github);
        let report = updater.update(&UpdateOptions {
            repo_root: args.repo.repo.clone(),
            workflows_path: args.repo.workflows_path.clone(),
            mode: UpdateMode::Status,
        })?;

        let updates_needed = report.entries.iter().filter(|entry| entry.update_needed).count();
        println!(
            "Scanned {} action references across {} workflow files. {} need changes.",
            report.references_scanned, report.workflow_files, updates_needed
        );

        for entry in &report.entries {
            println!(
                "- {}:{} {} current={} latest={} pinned={} status={}",
                entry.file.display(),
                entry.line_number,
                entry.action_slug,
                entry.current_version,
                entry.latest_version,
                entry.pinned,
                if entry.update_needed { "update-needed" } else { "current" }
            );
        }

        true
    } else {
        false
    };

    if targets.cargo {
        if printed_section {
            println!();
        }

        let crates_io = CratesIoClient::new(
            crates_api_base_url.unwrap_or_else(|| String::from("https://crates.io/api/v1")),
        )?;
        let updater = CargoUpdater::new(crates_io);
        let report = updater
            .update(&CargoUpdateOptions { repo_root: args.repo.repo, mode: UpdateMode::Status })?;

        let updates_needed = report.entries.iter().filter(|entry| entry.update_needed).count();
        println!(
            "Scanned {} cargo dependencies across {} manifests. {} need changes; {} are unmanaged.",
            report.dependencies_scanned,
            report.manifest_files,
            updates_needed,
            report.unmanaged_dependencies
        );

        for entry in &report.entries {
            let reason_suffix = entry
                .reason
                .as_deref()
                .map(|reason| format!(" reason={reason}"))
                .unwrap_or_default();
            println!(
                "- {} {} current={} latest={} managed={} status={}{}",
                entry.file.display(),
                entry.dependency_name,
                entry.current_requirement.as_deref().unwrap_or("n/a"),
                entry.latest_version.as_deref().unwrap_or("n/a"),
                entry.managed,
                cargo_status_label(entry),
                reason_suffix
            );
        }
    }

    Ok(())
}

const fn cargo_status_label(
    entry: &github_actions_maintainer::CargoDependencyEntry,
) -> &'static str {
    if !entry.managed {
        "unmanaged"
    } else if entry.update_needed {
        "update-needed"
    } else {
        "current"
    }
}

fn print_update_noop_summary(
    action_report: Option<&github_actions_maintainer::UpdateReport>,
    cargo_report: Option<&github_actions_maintainer::CargoUpdateReport>,
) {
    if let Some(report) = action_report {
        println!(
            "All scanned GitHub Actions are already current and pinned. Scanned {} references across {} workflow files.",
            report.references_scanned, report.workflow_files
        );
    }

    if let Some(report) = cargo_report {
        println!(
            "All managed cargo dependencies are already current. Scanned {} dependencies across {} manifests; {} unmanaged dependencies were skipped.",
            report.dependencies_scanned, report.manifest_files, report.unmanaged_dependencies
        );
    }
}

fn count_changed_files(file_updates: &[FileUpdate]) -> usize {
    let mut files = file_updates.iter().map(|update| update.file.as_path()).collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files.len()
}

fn print_update_changes(changes: &[UpdateChange]) {
    for change in changes {
        match change.line_number {
            Some(line_number) => println!(
                "- {} {}:{} {} {} -> {}",
                change.kind.label(),
                change.file.display(),
                line_number,
                change.subject,
                change.from_version,
                change.to_version
            ),
            None => println!(
                "- {} {} {} {} -> {}",
                change.kind.label(),
                change.file.display(),
                change.subject,
                change.from_version,
                change.to_version
            ),
        }
    }
}
