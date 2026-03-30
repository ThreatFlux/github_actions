use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use github_actions_maintainer::{
    GitHubClient, PinMode, PinOptions, PullRequestOptions, RemoteUpdatePublisher, UpdateMode,
    UpdateOptions, WorkflowPinner, WorkflowUpdater,
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
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Resolve floating GitHub Action refs to immutable commit SHAs.
    Pin(PinArgs),
    /// Update GitHub Actions to the latest release or tag, then pin them.
    Update(UpdateArgs),
    /// Report current and latest versions for GitHub Actions in workflows.
    Status(StatusArgs),
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
    #[arg(long, default_value = "dependencies,security")]
    labels: String,

    /// Pull request title for remote update mode.
    #[arg(long, default_value = "Update GitHub Actions dependencies")]
    title: String,

    /// Commit message for remote update mode.
    #[arg(long, default_value = "Update GitHub Actions dependencies")]
    commit_message: String,
}

#[derive(Debug, Args)]
struct PinArgs {
    #[command(flatten)]
    repo: RepoArgs,

    /// Show changes without rewriting files.
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    #[command(flatten)]
    repo: RepoArgs,

    /// Show available updates without rewriting files.
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[command(flatten)]
    repo: RepoArgs,

    #[arg(long, hide = true, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    _dry_run: bool,
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
        Commands::Update(args) => run_update(args, cli.github_api_base_url),
        Commands::Status(args) => run_status(args, cli.github_api_base_url),
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

fn run_update(args: UpdateArgs, github_api_base_url: Option<String>) -> Result<()> {
    let repo_args = args.repo;
    let repo_root = repo_args.repo.clone();
    let workflows_path = repo_args.workflows_path.clone();
    let token = repo_args.token.clone();
    let remote_mode = repo_args.create_pr;
    let github = GitHubClient::new(
        github_api_base_url.unwrap_or_else(|| String::from("https://api.github.com")),
        token,
    )?;
    let updater = WorkflowUpdater::new(github.clone());
    let report = updater.update(&UpdateOptions {
        repo_root: repo_root.clone(),
        workflows_path,
        mode: if args.dry_run || remote_mode { UpdateMode::DryRun } else { UpdateMode::Apply },
    })?;

    if report.changes.is_empty() {
        println!(
            "All scanned GitHub Actions are already current and pinned. Scanned {} references across {} workflow files.",
            report.references_scanned, report.workflow_files
        );
        return Ok(());
    }

    if remote_mode {
        if args.dry_run {
            println!(
                "Would create a pull request with {} action updates across {} files.",
                report.changes.len(),
                report.changed_files()
            );
        } else {
            let (owner, repo_name) = resolve_remote_repository(&repo_args)?;
            let publisher = RemoteUpdatePublisher::new(github);
            let result = publisher
                .publish(
                    &report.changes,
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

        for change in &report.changes {
            println!(
                "- {}:{} {} {} -> {}",
                change.file.display(),
                change.line_number,
                change.action_slug,
                change.from_version,
                change.to_sha
            );
        }

        return Ok(());
    }

    println!(
        "{} {} action references across {} files.",
        if args.dry_run { "Would update" } else { "Updated" },
        report.changes.len(),
        report.changed_files()
    );

    for change in &report.changes {
        println!(
            "- {}:{} {} {} -> {}",
            change.file.display(),
            change.line_number,
            change.action_slug,
            change.from_version,
            change.to_sha
        );
    }

    Ok(())
}

fn parse_labels(raw: &str) -> Vec<String> {
    raw.split(',').map(str::trim).filter(|label| !label.is_empty()).map(ToOwned::to_owned).collect()
}

fn resolve_remote_repository(args: &RepoArgs) -> Result<(String, String)> {
    match (args.owner.as_deref().map(str::trim), args.repo_name.as_deref().map(str::trim)) {
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

    anyhow::bail!("--owner and --repo-name are required when --create-pr is enabled")
}

fn run_status(args: StatusArgs, github_api_base_url: Option<String>) -> Result<()> {
    let github = GitHubClient::new(
        github_api_base_url.unwrap_or_else(|| String::from("https://api.github.com")),
        args.repo.token,
    )?;
    let updater = WorkflowUpdater::new(github);
    let report = updater.update(&UpdateOptions {
        repo_root: args.repo.repo,
        workflows_path: args.repo.workflows_path,
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

    Ok(())
}
