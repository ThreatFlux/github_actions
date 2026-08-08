mod input_env;
mod release_cli;

// Tests live in a sibling file to keep this module readable; they remain
// `super::`-scoped unit tests.
#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;

use std::{ffi::OsString, path::PathBuf};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use github_actions_maintainer::{
    CargoUpdateOptions, CargoUpdater, CratesIoClient, FileUpdate, GitHubClient, PinMode,
    PinOptions, PullRequestOptions, RemoteUpdatePublisher, UpdateChange, UpdateMode, UpdateOptions,
    WorkflowPinner, WorkflowUpdater,
};
use release_cli::{ReleaseArgs, run_release};

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
    #[arg(long, env = "INPUT_REPO", default_value = ".")]
    repo: PathBuf,

    /// Relative path to workflow files beneath the repository root.
    #[arg(long, env = "INPUT_WORKFLOWS-PATH", default_value = ".github/workflows")]
    workflows_path: PathBuf,

    /// GitHub token used to raise API rate limits; falls back to `GITHUB_TOKEN`.
    #[arg(long, env = "INPUT_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Repository owner used for remote PR creation; falls back to `OWNER`.
    #[arg(long, env = "INPUT_OWNER")]
    owner: Option<String>,

    /// Repository name used for remote PR creation; falls back to `REPO_NAME`.
    #[arg(long = "repo-name", env = "INPUT_REPO-NAME")]
    repo_name: Option<String>,

    /// Create a branch and pull request remotely instead of rewriting files locally.
    #[arg(long, env = "INPUT_CREATE-PR", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    create_pr: bool,

    /// Override the base branch for remote PR creation.
    #[arg(long, env = "INPUT_BASE-BRANCH")]
    base_branch: Option<String>,

    /// Override the update branch name for remote PR creation.
    #[arg(long, env = "INPUT_BRANCH-NAME")]
    branch_name: Option<String>,

    /// Labels to add to a created pull request, comma-separated.
    #[arg(long, env = "INPUT_LABELS", default_value = "dependencies")]
    labels: String,

    /// Pull request title for remote update mode.
    #[arg(long, env = "INPUT_TITLE", default_value = "Update dependencies")]
    title: String,

    /// Commit message for remote update mode.
    #[arg(long, env = "INPUT_COMMIT-MESSAGE", default_value = "Update dependencies")]
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
    #[arg(long, env = "INPUT_DRY-RUN", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    #[command(flatten)]
    repo: RepoArgs,

    #[command(flatten)]
    targets: TargetArgs,

    /// Show available updates without rewriting files.
    #[arg(long, env = "INPUT_DRY-RUN", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[command(flatten)]
    repo: RepoArgs,

    #[command(flatten)]
    targets: TargetArgs,

    #[arg(long, env = "INPUT_DRY-RUN", hide = true, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    _dry_run: bool,
}

#[derive(Debug, Args, Clone, Default)]
struct TargetArgs {
    /// Include GitHub Actions workflow updates.
    #[arg(long = "github-actions", env = "INPUT_GITHUB-ACTIONS", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    github_actions: bool,

    /// Include cargo package dependency updates.
    #[arg(long, env = "INPUT_CARGO", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    cargo: bool,

    /// Include both GitHub Actions and cargo package updates.
    #[arg(long, env = "INPUT_ALL", default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
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
    // SAFETY: `normalize_inputs` mutates the process environment. This is the
    // first statement in `main`, so no other thread exists yet and nothing else
    // has read the environment.
    let args = unsafe { input_env::normalize_inputs(std::env::args_os()) };

    if let Err(error) = run(args) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run(args: Vec<OsString>) -> Result<()> {
    let cli = Cli::parse_from(args);

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
