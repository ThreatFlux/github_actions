//! Resolution tests for the container action's `INPUT_*` environment
//! variables: explicit flags beat the environment, non-empty `INPUT_*` beats
//! the legacy variables, and empty values fall through to the per-subcommand
//! clap defaults.
//!
//! Every test here mutates the process environment, so all of them run through
//! `with_env`, which serializes on a shared lock and restores the environment.

use std::{ffi::OsString, path::PathBuf};

use clap::Parser as _;

use super::{
    Cli, Commands, RepoArgs, TargetArgs, input_env, release_cli::PhaseArg,
    release_cli::ReleaseArgs, release_cli::TagStyleArg,
};
use crate::input_env::testing::with_env;

/// Runs the real startup pipeline: normalize the environment and command line,
/// then parse.
fn parse(args: &[&str]) -> Cli {
    // SAFETY: callers run inside `with_env`, which holds the environment lock,
    // so no other test thread reads or writes the environment concurrently.
    let args = unsafe { input_env::normalize_inputs(args.iter().copied().map(OsString::from)) };
    Cli::parse_from(args)
}

fn repo_args(cli: Cli) -> RepoArgs {
    match cli.command {
        Commands::Pin(args) => args.repo,
        Commands::Update(args) => args.repo,
        Commands::Status(args) => args.repo,
        Commands::Policy(args) => args.repo,
        Commands::Release(_) => panic!("expected a maintainer subcommand"),
    }
}

fn target_args(cli: Cli) -> TargetArgs {
    match cli.command {
        Commands::Pin(args) => args.targets,
        Commands::Update(args) => args.targets,
        Commands::Status(args) => args.targets,
        Commands::Policy(_) | Commands::Release(_) => {
            panic!("expected a subcommand with dependency targets")
        }
    }
}

fn release_args(cli: Cli) -> ReleaseArgs {
    match cli.command {
        Commands::Release(args) => args,
        _ => panic!("expected the release subcommand"),
    }
}

fn update_dry_run(cli: Cli) -> bool {
    match cli.command {
        Commands::Update(args) => args.dry_run,
        _ => panic!("expected the update subcommand"),
    }
}

fn policy_args(cli: Cli) -> super::PolicyArgs {
    match cli.command {
        Commands::Policy(args) => args,
        _ => panic!("expected the policy subcommand"),
    }
}

#[test]
fn policy_scan_toggles_default_on_and_fail_on_findings_defaults_off() {
    with_env(&[], || {
        let args = policy_args(parse(&["bin", "policy"]));

        assert!(args.check_scripts);
        assert!(args.check_policies);
        assert!(!args.fail_on_findings);
    });
}

#[test]
fn policy_scan_toggles_resolve_from_input_env() {
    with_env(
        &[
            ("INPUT_CHECK-SCRIPTS", "false"),
            ("INPUT_CHECK-POLICIES", "false"),
            ("INPUT_FAIL-ON-FINDINGS", "true"),
        ],
        || {
            let args = policy_args(parse(&["bin", "policy"]));

            assert!(!args.check_scripts);
            assert!(!args.check_policies);
            assert!(args.fail_on_findings);
        },
    );
}

#[test]
fn explicit_policy_flags_beat_input_env() {
    with_env(&[("INPUT_FAIL-ON-FINDINGS", "false")], || {
        let args = policy_args(parse(&["bin", "policy", "--fail-on-findings", "true"]));

        assert!(args.fail_on_findings);
    });
}

#[test]
fn input_env_resolves_options_the_action_no_longer_passes_as_flags() {
    with_env(
        &[
            ("INPUT_REPO", "/checkout"),
            ("INPUT_WORKFLOWS-PATH", ".github/flows"),
            ("INPUT_BRANCH-NAME", "automation/deps"),
            ("INPUT_LABELS", "deps,security"),
            ("INPUT_TITLE", "Refresh pinned actions"),
            ("INPUT_BASE-BRANCH", "dev"),
        ],
        || {
            let args = repo_args(parse(&["bin", "update"]));

            assert_eq!(args.repo, PathBuf::from("/checkout"));
            assert_eq!(args.workflows_path, PathBuf::from(".github/flows"));
            assert_eq!(args.branch_name.as_deref(), Some("automation/deps"));
            assert_eq!(args.labels, "deps,security");
            assert_eq!(args.title, "Refresh pinned actions");
            assert_eq!(args.base_branch.as_deref(), Some("dev"));
        },
    );
}

#[test]
fn explicit_flag_beats_input_env() {
    with_env(&[("INPUT_TITLE", "from environment"), ("INPUT_TOKEN", "ghp_env")], || {
        let args =
            repo_args(parse(&["bin", "update", "--title", "from flag", "--token", "ghp_flag"]));

        assert_eq!(args.title, "from flag");
        assert_eq!(args.token.as_deref(), Some("ghp_flag"));
    });
}

#[test]
fn input_env_wins_over_legacy_env() {
    with_env(
        &[
            ("INPUT_TOKEN", "ghp_input"),
            ("GITHUB_TOKEN", "ghp_legacy"),
            ("INPUT_OWNER", "threatflux"),
            ("OWNER", "legacy-owner"),
            ("INPUT_REPO-NAME", "github_actions"),
            ("REPO_NAME", "legacy-repo"),
        ],
        || {
            let args = repo_args(parse(&["bin", "status"]));

            assert_eq!(args.token.as_deref(), Some("ghp_input"));
            assert_eq!(args.owner.as_deref(), Some("threatflux"));
            assert_eq!(args.repo_name.as_deref(), Some("github_actions"));
        },
    );
}

#[test]
fn legacy_env_still_resolves_token_owner_and_repo_name() {
    with_env(
        &[("GITHUB_TOKEN", "ghp_legacy"), ("OWNER", "threatflux"), ("REPO_NAME", "github_actions")],
        || {
            let args = repo_args(parse(&["bin", "update"]));
            assert_eq!(args.token.as_deref(), Some("ghp_legacy"));
            assert_eq!(args.owner.as_deref(), Some("threatflux"));
            assert_eq!(args.repo_name.as_deref(), Some("github_actions"));

            let args = release_args(parse(&["bin", "release"]));
            assert_eq!(args.token.as_deref(), Some("ghp_legacy"));
            assert_eq!(args.owner.as_deref(), Some("threatflux"));
            assert_eq!(args.repo_name.as_deref(), Some("github_actions"));
        },
    );
}

#[test]
fn empty_input_env_falls_through_to_the_legacy_env() {
    with_env(&[("INPUT_TOKEN", "   "), ("GITHUB_TOKEN", "ghp_legacy")], || {
        let args = repo_args(parse(&["bin", "pin"]));
        assert_eq!(args.token.as_deref(), Some("ghp_legacy"));
    });
}

#[test]
fn empty_input_env_falls_through_to_the_subcommand_default() {
    with_env(
        &[
            ("INPUT_REPO", ""),
            ("INPUT_WORKFLOWS-PATH", ""),
            ("INPUT_LABELS", "  "),
            ("INPUT_TITLE", ""),
            ("INPUT_TAG-PREFIX", ""),
            ("INPUT_RELEASE-BRANCH", ""),
            ("INPUT_NOTES-FILE", ""),
        ],
        || {
            let args = repo_args(parse(&["bin", "update"]));
            assert_eq!(args.repo, PathBuf::from("."));
            assert_eq!(args.workflows_path, PathBuf::from(".github/workflows"));
            assert_eq!(args.labels, "dependencies");
            assert_eq!(args.title, "Update dependencies");

            let args = release_args(parse(&["bin", "release"]));
            assert_eq!(args.repo, PathBuf::from("."));
            assert_eq!(args.tag_prefix, "v");
            assert_eq!(args.release_branch, "automation/release");
            assert_eq!(args.notes_file, PathBuf::from("release_notes.md"));
        },
    );
}

#[test]
fn empty_input_env_leaves_optional_values_unset() {
    with_env(
        &[
            ("INPUT_TOKEN", ""),
            ("INPUT_OWNER", " "),
            ("INPUT_REPO-NAME", ""),
            ("INPUT_BASE-BRANCH", ""),
            ("INPUT_BRANCH-NAME", "\t"),
        ],
        || {
            let args = repo_args(parse(&["bin", "update"]));
            assert_eq!(args.token, None);
            assert_eq!(args.owner, None);
            assert_eq!(args.repo_name, None);
            assert_eq!(args.base_branch, None);
            assert_eq!(args.branch_name, None);
        },
    );
}

#[test]
fn commit_message_defaults_stay_per_subcommand() {
    with_env(&[("INPUT_COMMIT-MESSAGE", "")], || {
        assert_eq!(repo_args(parse(&["bin", "update"])).commit_message, "Update dependencies");
        assert_eq!(
            release_args(parse(&["bin", "release"])).commit_message,
            "chore: release v{version}"
        );
    });
}

#[test]
fn release_parses_without_extra_files() {
    // Regression: an empty `default_value` combined with `value_delimiter`
    // made clap reject every `release` parse, not just ones passing the flag.
    with_env(&[], || {
        assert!(release_args(parse(&["bin", "release"])).extra_files.is_empty());
    });
}

#[test]
fn release_extra_files_resolve_from_input_env() {
    with_env(&[("INPUT_EXTRA-FILES", "runtime/Dockerfile,release/Dockerfile")], || {
        assert_eq!(
            release_args(parse(&["bin", "release"])).extra_files,
            vec![PathBuf::from("runtime/Dockerfile"), PathBuf::from("release/Dockerfile")]
        );
    });
}

#[test]
fn empty_extra_files_input_env_yields_no_files() {
    with_env(&[("INPUT_EXTRA-FILES", "")], || {
        assert!(release_args(parse(&["bin", "release"])).extra_files.is_empty());
    });
}

#[test]
fn release_phase_defaults_to_a_single_run() {
    with_env(&[], || {
        assert_eq!(release_args(parse(&["bin", "release"])).phase, PhaseArg::All);
    });
}

#[test]
fn release_phase_resolves_from_input_env() {
    with_env(&[("INPUT_PHASE", "bump")], || {
        assert_eq!(release_args(parse(&["bin", "release"])).phase, PhaseArg::Bump);
    });
    with_env(&[("INPUT_PHASE", "tag")], || {
        assert_eq!(release_args(parse(&["bin", "release"])).phase, PhaseArg::Tag);
    });
}

#[test]
fn empty_phase_input_env_still_releases_in_one_run() {
    // action.yml passes every optional input as an empty string when unset, so
    // an unset phase must not turn into a partial release.
    with_env(&[("INPUT_PHASE", "")], || {
        assert_eq!(release_args(parse(&["bin", "release"])).phase, PhaseArg::All);
    });
}

#[test]
fn commit_message_input_env_overrides_both_subcommand_defaults() {
    with_env(&[("INPUT_COMMIT-MESSAGE", "chore: sync deps")], || {
        assert_eq!(repo_args(parse(&["bin", "update"])).commit_message, "chore: sync deps");
        assert_eq!(release_args(parse(&["bin", "release"])).commit_message, "chore: sync deps");
    });
}

#[test]
fn boolean_flags_parse_from_input_env() {
    with_env(
        &[
            ("INPUT_DRY-RUN", "true"),
            ("INPUT_CREATE-PR", "true"),
            ("INPUT_GITHUB-ACTIONS", "true"),
            ("INPUT_CARGO", "false"),
            ("INPUT_ALL", "true"),
        ],
        || {
            assert!(update_dry_run(parse(&["bin", "update"])));
            assert!(repo_args(parse(&["bin", "update"])).create_pr);

            let targets = target_args(parse(&["bin", "status"]));
            assert!(targets.github_actions);
            assert!(!targets.cargo);
            assert!(targets.all);
        },
    );
}

#[test]
fn boolean_flags_read_false_from_input_env() {
    with_env(&[("INPUT_DRY-RUN", "false"), ("INPUT_CREATE-PR", "false")], || {
        assert!(!update_dry_run(parse(&["bin", "update"])));
        assert!(!repo_args(parse(&["bin", "update"])).create_pr);
    });
}

#[test]
fn boolean_flag_on_the_command_line_beats_input_env() {
    with_env(&[("INPUT_DRY-RUN", "true")], || {
        assert!(!update_dry_run(parse(&["bin", "update", "--dry-run", "false"])));
    });
}

#[test]
fn release_only_options_resolve_from_hyphenated_input_env() {
    with_env(
        &[
            ("INPUT_BUMP", "minor"),
            ("INPUT_TAG-PREFIX", "release-"),
            ("INPUT_TAG-STYLE", "lightweight"),
            ("INPUT_UPDATE-MAJOR-ALIAS", "true"),
            ("INPUT_NOTES-FILE", "notes/out.md"),
            ("INPUT_RELEASE-BRANCH", "automation/release/next"),
            ("INPUT_CREATE-PR", "true"),
        ],
        || {
            let args = release_args(parse(&["bin", "release"]));

            assert_eq!(args.bump, super::release_cli::BumpArg::Minor);
            assert_eq!(args.tag_prefix, "release-");
            assert_eq!(args.tag_style, TagStyleArg::Lightweight);
            assert!(args.update_major_alias);
            assert_eq!(args.notes_file, PathBuf::from("notes/out.md"));
            assert_eq!(args.release_branch, "automation/release/next");
            assert!(args.create_pr);
        },
    );
}

#[test]
fn empty_command_line_values_are_unset_in_every_subcommand() {
    with_env(&[], || {
        for command in ["pin", "update", "status"] {
            let args = repo_args(parse(&[
                "bin",
                command,
                "--token",
                "",
                "--owner",
                "  ",
                "--repo-name=",
                "--base-branch",
                "",
                "--branch-name",
                "",
            ]));

            assert_eq!(args.token, None, "{command}");
            assert_eq!(args.owner, None, "{command}");
            assert_eq!(args.repo_name, None, "{command}");
            assert_eq!(args.base_branch, None, "{command}");
            assert_eq!(args.branch_name, None, "{command}");
        }

        let args = release_args(parse(&[
            "bin",
            "release",
            "--token",
            "",
            "--owner",
            "  ",
            "--repo-name=",
            "--base-branch",
            "",
        ]));

        assert_eq!(args.token, None);
        assert_eq!(args.owner, None);
        assert_eq!(args.repo_name, None);
        assert_eq!(args.base_branch, None);
    });
}

#[test]
fn empty_command_line_values_fall_through_to_input_env() {
    with_env(&[("INPUT_OWNER", "threatflux"), ("INPUT_TITLE", "from environment")], || {
        let args = repo_args(parse(&["bin", "update", "--owner", "", "--title", ""]));

        assert_eq!(args.owner.as_deref(), Some("threatflux"));
        assert_eq!(args.title, "from environment");
    });
}
