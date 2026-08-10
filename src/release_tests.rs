use std::{
    fs,
    path::{Path, PathBuf},
};

use mockito::{Matcher, Mock, Server, ServerGuard};
use tempfile::{TempDir, tempdir};

use super::{ReleaseOptions, ReleaseOutcome, ReleasePublisher, TagStyle};
use crate::{GitHubClient, conventional::BumpLevel};

// Extra-file staging has its own mock scaffolding, so it lives in a sibling
// file to keep both modules within the repository's file-size lint budget.
#[path = "release_extra_files_tests.rs"]
mod extra_files;

const FEAT_AND_FIX: &str = r#"{"total_commits":2,"commits":[{"sha":"feataaaaaaa","commit":{"message":"feat: add thing"},"parents":[{}]},{"sha":"fixbbbbbbbb","commit":{"message":"fix: repair thing"},"parents":[{}]}]}"#;
const CHORE_ONLY: &str = r#"{"total_commits":1,"commits":[{"sha":"choreaaaaaa","commit":{"message":"chore: tidy"},"parents":[{}]}]}"#;
const TAGS_V023: &str = r#"[{"name":"v0.2.3","commit":{"sha":"tagsha"}}]"#;

fn write_fixture_repo() -> TempDir {
    let temp_dir = tempdir().expect("tempdir");
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.2.3\"\n",
    )
    .expect("write Cargo.toml");
    temp_dir
}

fn options(repo_root: &Path) -> ReleaseOptions {
    ReleaseOptions {
        repo_root: repo_root.to_path_buf(),
        owner: String::from("acme"),
        repo: String::from("demo"),
        base_branch: Some(String::from("main")),
        bump: None,
        tag_prefix: String::from("v"),
        tag_style: TagStyle::Annotated,
        update_major_alias: false,
        commit_message: String::from("chore: release v{version}"),
        create_pr: false,
        release_branch: String::from("automation/release"),
        dry_run: false,
        extra_files: Vec::new(),
    }
}

fn mock_tag_object(server: &mut ServerGuard) -> Mock {
    server
        .mock("POST", "/repos/acme/demo/git/tags")
        .with_status(201)
        .with_body(r#"{"sha":"tagobjectsha"}"#)
        .create()
}

fn publisher(server: &ServerGuard) -> ReleasePublisher {
    let client = GitHubClient::new(server.url(), Some(String::from("ghp_testtoken")))
        .expect("github client");
    ReleasePublisher::new(client)
}

fn mock_head_ref(server: &mut ServerGuard, sha: &str, hits: usize) -> Mock {
    server
        .mock("GET", "/repos/acme/demo/git/ref/heads/main")
        .expect(hits)
        .with_status(200)
        .with_body(format!(r#"{{"ref":"refs/heads/main","object":{{"sha":"{sha}"}}}}"#))
        .create()
}

fn mock_tags(server: &mut ServerGuard, body: &str) -> Mock {
    server
        .mock("GET", "/repos/acme/demo/tags?per_page=100&page=1")
        .with_status(200)
        .with_body(body)
        .create()
}

fn mock_compare(server: &mut ServerGuard, body: &str) -> Mock {
    server
        .mock("GET", "/repos/acme/demo/compare/v0.2.3...basecommitsha?per_page=100&page=1")
        .with_status(200)
        .with_body(body)
        .create()
}

fn mock_tag_lookup(server: &mut ServerGuard, tag: &str, status: usize, body: &str) -> Mock {
    server
        .mock("GET", format!("/repos/acme/demo/git/ref/tags/{tag}").as_str())
        .with_status(status)
        .with_body(body)
        .create()
}

/// Standard read-side mocks: head ref, tag listing, compare range, and a
/// missing v0.3.0 release tag.
fn mock_analysis(server: &mut ServerGuard, head_hits: usize, compare_body: &str) -> Vec<Mock> {
    vec![
        mock_head_ref(server, "basecommitsha", head_hits),
        mock_tags(server, TAGS_V023),
        mock_compare(server, compare_body),
        mock_tag_lookup(server, "v0.3.0", 404, r#"{"message":"Not Found"}"#),
    ]
}

fn mock_build_chain(server: &mut ServerGuard) -> Vec<Mock> {
    vec![
        server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"sha":"basecommitsha","tree":{"sha":"basetreesha"}}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/trees")
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create(),
    ]
}

fn mock_finalize_chain(server: &mut ServerGuard, tag: &str) -> Vec<Mock> {
    vec![
        server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .with_status(200)
            .with_body(r#"{"ref":"refs/heads/main"}"#)
            .create(),
        mock_tag_object(server),
        server
            .mock("POST", "/repos/acme/demo/git/refs")
            .with_status(201)
            .with_body(format!(r#"{{"ref":"refs/tags/{tag}"}}"#))
            .create(),
        server
            .mock("POST", "/repos/acme/demo/releases")
            .with_status(201)
            .with_body(format!(
                r#"{{"html_url":"https://github.com/acme/demo/releases/tag/{tag}"}}"#
            ))
            .create(),
    ]
}

fn mock_no_mutations(server: &mut ServerGuard) -> Vec<Mock> {
    vec![
        server.mock("POST", "/repos/acme/demo/git/blobs").expect(0).create(),
        server.mock("PATCH", "/repos/acme/demo/git/refs/heads/main").expect(0).create(),
        server.mock("POST", "/repos/acme/demo/git/refs").expect(0).create(),
        server.mock("POST", "/repos/acme/demo/releases").expect(0).create(),
    ]
}

/// Full pipeline mocks asserting the exact payloads the publisher sends.
fn mock_strict_pipeline(server: &mut ServerGuard) -> Vec<Mock> {
    vec![
        server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"sha":"basecommitsha","tree":{"sha":"basetreesha"}}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/blobs")
            .match_body(Matcher::Regex(r#"version = \\"0\.3\.0\\""#.into()))
            .expect(1)
            .with_status(201)
            .with_body(r#"{"sha":"blobsha"}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/trees")
            .match_body(Matcher::Regex(r#""path":"Cargo\.toml""#.into()))
            .with_status(201)
            .with_body(r#"{"sha":"treesha"}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/commits")
            .match_body(Matcher::Regex(r#""message":"chore: release v0\.3\.0""#.into()))
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create(),
        server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .match_body(Matcher::Regex(r#""sha":"newcommitsha""#.into()))
            .with_status(200)
            .with_body(r#"{"ref":"refs/heads/main"}"#)
            .create(),
    ]
}

/// Strict finalize mocks: annotated tag object, tag ref at the object SHA,
/// and the release payload.
fn mock_strict_finalize(server: &mut ServerGuard) -> Vec<Mock> {
    vec![
        server
            .mock("POST", "/repos/acme/demo/git/tags")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#""tag":"v0\.3\.0""#.into()),
                Matcher::Regex(r#""object":"newcommitsha""#.into()),
                Matcher::Regex(r#""type":"commit""#.into()),
            ]))
            .with_status(201)
            .with_body(r#"{"sha":"tagobjectsha"}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/refs")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#""ref":"refs/tags/v0\.3\.0""#.into()),
                Matcher::Regex(r#""sha":"tagobjectsha""#.into()),
            ]))
            .with_status(201)
            .with_body(r#"{"ref":"refs/tags/v0.3.0"}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/releases")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#""tag_name":"v0\.3\.0""#.into()),
                Matcher::Regex(r#""target_commitish":"newcommitsha""#.into()),
                Matcher::Regex("### Features".into()),
            ]))
            .with_status(201)
            .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.3.0"}"#)
            .create(),
    ]
}

#[test]
fn release_creates_commit_tag_and_release() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _pipeline = mock_strict_pipeline(&mut server);
    let _finalize = mock_strict_finalize(&mut server);

    let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.current_version, "0.2.3");
    assert_eq!(report.next_version.as_deref(), Some("0.3.0"));
    assert_eq!(report.bump, Some(BumpLevel::Minor));
    assert_eq!(report.tag.as_deref(), Some("v0.3.0"));
    assert_eq!(report.commit_sha.as_deref(), Some("newcommitsha"));
    assert_eq!(
        report.release_url.as_deref(),
        Some("https://github.com/acme/demo/releases/tag/v0.3.0")
    );
    assert_eq!(report.files_updated.len(), 1);
}

#[test]
fn release_dry_run_performs_no_mutations_and_renders_outputs() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 1, FEAT_AND_FIX);
    let _no_mutations = mock_no_mutations(&mut server);

    let mut release_options = options(temp_dir.path());
    release_options.dry_run = true;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::DryRun);
    assert!(report.notes.as_deref().is_some_and(|notes| notes.contains("### Features")));
    assert_eq!(report.files_updated.len(), 1);
    assert_eq!(
        report.github_outputs(Path::new("release_notes.md")),
        "released=false\nversion=0.3.0\ntag=v0.3.0\nrelease-url=\nrelease-pr-number=\nrelease-pr-url=\nrelease-branch=\nnotes-file=release_notes.md\n"
    );
}

#[test]
fn release_lightweight_tags_point_directly_at_the_commit() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _build = mock_build_chain(&mut server);
    let _advance = server
        .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
        .with_status(200)
        .with_body(r#"{"ref":"refs/heads/main"}"#)
        .create();
    let _no_tag_object = server.mock("POST", "/repos/acme/demo/git/tags").expect(0).create();
    let _tag_ref = server
        .mock("POST", "/repos/acme/demo/git/refs")
        .match_body(Matcher::AllOf(vec![
            Matcher::Regex(r#""ref":"refs/tags/v0\.3\.0""#.into()),
            Matcher::Regex(r#""sha":"newcommitsha""#.into()),
        ]))
        .with_status(201)
        .with_body(r#"{"ref":"refs/tags/v0.3.0"}"#)
        .create();
    let _release = server
        .mock("POST", "/repos/acme/demo/releases")
        .with_status(201)
        .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.3.0"}"#)
        .create();

    let mut release_options = options(temp_dir.path());
    release_options.tag_style = TagStyle::Lightweight;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
}

#[test]
fn release_creates_automated_release_branch_and_pull_request() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _build = mock_build_chain(&mut server);
    let _release_branch_missing = server
        .mock("GET", "/repos/acme/demo/git/ref/heads/automation/release")
        .with_status(404)
        .with_body(r#"{"message":"Not Found"}"#)
        .create();
    let _create_branch = server
        .mock("POST", "/repos/acme/demo/git/refs")
        .match_body(Matcher::Regex(
            r#""ref":"refs/heads/automation/release".*"sha":"newcommitsha""#.into(),
        ))
        .with_status(201)
        .with_body(r#"{"ref":"refs/heads/automation/release"}"#)
        .create();
    let _find_pr = server
        .mock(
            "GET",
            "/repos/acme/demo/pulls?state=open&head=acme%3Aautomation%2Frelease&base=main&per_page=100",
        )
        .with_status(200)
        .with_body("[]")
        .create();
    let _create_pr = server
        .mock("POST", "/repos/acme/demo/pulls")
        .match_body(Matcher::Regex(r#""head":"automation/release".*"base":"main""#.into()))
        .with_status(201)
        .with_body(r#"{"number":42,"html_url":"https://github.com/acme/demo/pull/42"}"#)
        .create();

    let mut release_options = options(temp_dir.path());
    release_options.create_pr = true;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::PullRequestCreated);
    assert_eq!(report.pull_request_number, Some(42));
    assert_eq!(report.pull_request_url.as_deref(), Some("https://github.com/acme/demo/pull/42"));
    assert_eq!(report.release_branch.as_deref(), Some("automation/release"));
    assert_eq!(report.release_url, None);
}

#[test]
fn release_skips_when_no_commits_warrant_a_release() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _tags = mock_tags(&mut server, TAGS_V023);
    let _compare = mock_compare(&mut server, CHORE_ONLY);
    let _no_mutations = mock_no_mutations(&mut server);

    let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedNoReleasableChanges);
    assert_eq!(report.next_version, None);
    assert_eq!(report.commits_analyzed, 1);
}

#[test]
fn release_bump_override_releases_without_qualifying_commits() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 2);
    let _tags = mock_tags(&mut server, TAGS_V023);
    let _compare = mock_compare(&mut server, CHORE_ONLY);
    let _tag_missing = mock_tag_lookup(&mut server, "v0.2.4", 404, r#"{"message":"Not Found"}"#);
    let _build = mock_build_chain(&mut server);
    let _finalize = mock_finalize_chain(&mut server, "v0.2.4");

    let mut release_options = options(temp_dir.path());
    release_options.bump = Some(BumpLevel::Patch);
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.next_version.as_deref(), Some("0.2.4"));
}

#[test]
fn release_skips_when_tag_already_exists() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _tags = mock_tags(&mut server, TAGS_V023);
    let _compare = mock_compare(&mut server, FEAT_AND_FIX);
    let _tag_exists = mock_tag_lookup(
        &mut server,
        "v0.3.0",
        200,
        r#"{"ref":"refs/tags/v0.3.0","object":{"sha":"existingsha"}}"#,
    );
    let _no_mutations = mock_no_mutations(&mut server);

    let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedTagExists);
    assert_eq!(report.tag.as_deref(), Some("v0.3.0"));
}

#[test]
fn release_skips_when_branch_advances_before_the_ref_update() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    // First read pins the analysis head; the re-read sees a newer commit.
    let _initial_head = mock_head_ref(&mut server, "basecommitsha", 1);
    let _moved_head = mock_head_ref(&mut server, "advancedsha", 1);
    let _tags = mock_tags(&mut server, TAGS_V023);
    let _compare = mock_compare(&mut server, FEAT_AND_FIX);
    let _tag_missing = mock_tag_lookup(&mut server, "v0.3.0", 404, r#"{"message":"Not Found"}"#);
    let _build = mock_build_chain(&mut server);
    let _no_advance =
        server.mock("PATCH", "/repos/acme/demo/git/refs/heads/main").expect(0).create();
    let _no_release = server.mock("POST", "/repos/acme/demo/releases").expect(0).create();

    let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedRace);
    assert_eq!(report.commit_sha, None);
}

#[test]
fn release_skips_when_the_ref_update_is_not_a_fast_forward() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _build = mock_build_chain(&mut server);
    let _rejected = server
        .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
        .with_status(422)
        .with_body(r#"{"message":"Update is not a fast forward"}"#)
        .create();
    let _no_release = server.mock("POST", "/repos/acme/demo/releases").expect(0).create();

    let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedRace);
}

#[test]
fn release_handles_repositories_without_tags() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _head = mock_head_ref(&mut server, "basecommitsha", 2);
    let _tags = mock_tags(&mut server, "[]");
    let _commits = server
        .mock("GET", "/repos/acme/demo/commits?sha=basecommitsha&per_page=100&page=1")
        .with_status(200)
        .with_body(r#"[{"sha":"feataaaaaaa","commit":{"message":"feat: initial"},"parents":[]}]"#)
        .create();
    let _tag_missing = mock_tag_lookup(&mut server, "v0.3.0", 404, r#"{"message":"Not Found"}"#);
    let _build = mock_build_chain(&mut server);
    let _finalize = mock_finalize_chain(&mut server, "v0.3.0");

    let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.next_version.as_deref(), Some("0.3.0"));
}

#[test]
fn release_moves_an_existing_major_alias_with_force() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _build = mock_build_chain(&mut server);
    let _finalize = mock_finalize_chain(&mut server, "v0.3.0");
    let _alias_exists = mock_tag_lookup(
        &mut server,
        "v0",
        200,
        r#"{"ref":"refs/tags/v0","object":{"sha":"oldaliassha"}}"#,
    );
    let _alias_moved = server
        .mock("PATCH", "/repos/acme/demo/git/refs/tags/v0")
        .match_body(Matcher::AllOf(vec![
            Matcher::Regex(r#""sha":"newcommitsha""#.into()),
            Matcher::Regex(r#""force":true"#.into()),
        ]))
        .expect(1)
        .with_status(200)
        .with_body(r#"{"ref":"refs/tags/v0"}"#)
        .create();

    let mut release_options = options(temp_dir.path());
    release_options.update_major_alias = true;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.major_alias.as_deref(), Some("v0"));
}
