//! Hosted end-to-end CLI/environment to release-request draft controls.

use std::fs;

use clap::Parser as _;
use mockito::{Matcher, Mock, Server, ServerGuard};

use super::{Cli, PhaseArg, parse, release_args, with_env};

#[derive(Clone, Copy)]
enum Case {
    Draft,
    Published,
    DryRun,
    ExistingTag,
    HeadMismatch,
    Rejected,
    WrongResponse,
}

fn read_mocks(server: &mut ServerGuard, case: Case, writes: usize) -> Vec<Mock> {
    let stopped = matches!(case, Case::HeadMismatch);
    let head = server
        .mock("GET", "/repos/acme/demo/git/ref/heads/main")
        .expect(if writes == 1 { 2 } else { 1 })
        .with_status(200)
        .with_body(if stopped {
            r#"{"object":{"sha":"advancedsha"}}"#
        } else {
            r#"{"object":{"sha":"basecommitsha"}}"#
        })
        .create();
    let tags = server
        .mock("GET", "/repos/acme/demo/tags?per_page=100&page=1")
        .expect(usize::from(!stopped))
        .with_status(200)
        .with_body(r#"[{"name":"v0.2.3","commit":{"sha":"oldtagsha"}}]"#)
        .create();
    let compare = server.mock("GET", "/repos/acme/demo/compare/v0.2.3...basecommitsha?per_page=100&page=1")
        .expect(usize::from(!stopped)).with_status(200)
        .with_body(r#"{"total_commits":1,"commits":[{"sha":"fixsha","commit":{"message":"fix: native release"},"parents":[{}]}]}"#).create();
    let existing = matches!(case, Case::ExistingTag);
    let tag = server
        .mock("GET", "/repos/acme/demo/git/ref/tags/v0.2.4")
        .expect(usize::from(!stopped))
        .with_status(if existing { 200 } else { 404 })
        .with_body(if existing {
            r#"{"object":{"sha":"existingtagsha"}}"#
        } else {
            r#"{"message":"Not Found"}"#
        })
        .create();
    vec![head, tags, compare, tag]
}

fn exercise(case: Case, environment: &[(&str, &str)], explicit: Option<&str>) {
    with_env(environment, || {
        let root = tempfile::tempdir().expect("fixture repository");
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.2.4\"\n",
        )
        .expect("fixture manifest");
        let mut arguments = vec![
            "bin",
            "release",
            "--owner",
            "acme",
            "--repo-name",
            "demo",
            "--base-branch",
            "main",
            "--token",
            "test-owned-token",
        ];
        if let Some(value) = explicit {
            arguments.extend(["--draft-release", value]);
        }
        let mut args = release_args(parse(&arguments));
        let requested = !matches!(case, Case::Published);
        assert_eq!(args.draft_release, requested);
        args.repo = root.path().to_path_buf();
        args.notes_file = root.path().join("notes.md");
        args.github_output = Some(root.path().join("outputs"));
        args.phase = PhaseArg::Tag;
        args.dry_run = matches!(case, Case::DryRun);
        args.expected_head = Some("basecommitsha".to_owned());

        let mut server = Server::new();
        let writes =
            usize::from(!matches!(case, Case::DryRun | Case::ExistingTag | Case::HeadMismatch));
        let mut mocks = read_mocks(&mut server, case, writes);
        let tag_object = server
            .mock("POST", "/repos/acme/demo/git/tags")
            .match_body(Matcher::Regex(r#""object":"basecommitsha""#.to_owned()))
            .expect(writes)
            .with_status(201)
            .with_body(r#"{"sha":"tagobjectsha"}"#)
            .create();
        let tag_ref = server
            .mock("POST", "/repos/acme/demo/git/refs")
            .match_body(Matcher::Regex(r#""sha":"tagobjectsha""#.to_owned()))
            .expect(writes)
            .with_status(201)
            .with_body(r#"{"ref":"refs/tags/v0.2.4"}"#)
            .create();
        let returned_draft = requested && !matches!(case, Case::WrongResponse);
        let release = server.mock("POST", "/repos/acme/demo/releases")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(format!(r#""draft":{requested}"#)),
                Matcher::Regex(r#""tag_name":"v0.2.4""#.to_owned()),
                Matcher::Regex(r#""target_commitish":"basecommitsha""#.to_owned()),
            ])).expect(writes).with_status(if matches!(case, Case::Rejected) { 422 } else { 201 })
            .with_body(format!(r#"{{"html_url":"https://github.com/acme/demo/releases/tag/v0.2.4","draft":{returned_draft}}}"#)).create();
        let no_publish = server.mock("PATCH", Matcher::Any).expect(0).create();
        let no_delete = server.mock("DELETE", Matcher::Any).expect(0).create();
        let no_dispatch = server
            .mock("POST", Matcher::Regex("/actions/workflows/".to_owned()))
            .expect(0)
            .create();

        let result = crate::release_cli::run_release(args, Some(server.url()));
        if matches!(case, Case::HeadMismatch | Case::Rejected | Case::WrongResponse) {
            let error = result.expect_err("refusal remains blocking");
            if matches!(case, Case::HeadMismatch) {
                assert!(error.to_string().contains("does not match expected head"));
            } else if matches!(case, Case::WrongResponse) {
                assert!(error.to_string().contains("requested draft state"));
            }
            assert!(!root.path().join("outputs").exists());
        } else {
            result.expect("release result");
            let output = fs::read_to_string(root.path().join("outputs")).expect("outputs");
            assert!(output.contains(&format!("released={}\n", writes == 1)));
            assert!(output.contains(&format!("release-draft={}\n", writes == 1 && requested)));
            if writes == 1 {
                assert!(output.contains("commit=basecommitsha\n"));
            }
        }
        mocks.extend([tag_object, tag_ref, release, no_publish, no_delete, no_dispatch]);
        for mock in mocks {
            mock.assert();
        }
        drop(server);
    });
}

#[test]
fn draft_input_environment_reaches_the_actual_release_payload() {
    exercise(Case::Draft, &[("INPUT_DRAFT-RELEASE", "true")], None);
}

#[test]
fn draft_explicit_cli_flag_reaches_the_actual_release_payload() {
    exercise(Case::Draft, &[], Some("true"));
}

#[test]
fn omitted_draft_keeps_the_published_cargo_default() {
    exercise(Case::Published, &[], None);
}

#[test]
fn explicit_false_beats_a_draft_input_environment() {
    exercise(Case::Published, &[("INPUT_DRAFT-RELEASE", "true")], Some("false"));
}

#[test]
fn draft_dry_run_never_creates_a_tag_or_release() {
    exercise(Case::DryRun, &[("INPUT_DRAFT-RELEASE", "true")], None);
}

#[test]
fn existing_tag_never_recreates_or_republishes_any_release() {
    // The guard does not infer draft/completed/immutable release readiness
    // from a tag, and never PATCHes or DELETEs an existing release.
    exercise(Case::ExistingTag, &[("INPUT_DRAFT-RELEASE", "true")], None);
}

#[test]
fn draft_mode_retains_the_exact_head_guard() {
    exercise(Case::HeadMismatch, &[("INPUT_DRAFT-RELEASE", "true")], None);
}

#[test]
fn rejected_release_is_not_retried_as_an_overwrite_or_publication() {
    exercise(Case::Rejected, &[("INPUT_DRAFT-RELEASE", "true")], None);
}

#[test]
fn draft_response_state_must_match_the_request() {
    exercise(Case::WrongResponse, &[("INPUT_DRAFT-RELEASE", "true")], None);
}

#[test]
fn draft_boolean_values_are_strict_and_bare_switch_is_true() {
    with_env(&[], || {
        let args = release_args(parse(&["bin", "release", "--draft-release"]));
        assert!(args.draft_release);
        assert!(Cli::try_parse_from(["bin", "release", "--draft-release", "perhaps"]).is_err());
    });
    with_env(&[("INPUT_DRAFT-RELEASE", "perhaps")], || {
        assert!(Cli::try_parse_from(["bin", "release"]).is_err());
    });
}
