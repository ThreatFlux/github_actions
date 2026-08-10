use std::{fs, path::PathBuf};

use mockito::{Matcher, Mock, Server, ServerGuard};

use super::{
    FEAT_AND_FIX, ReleaseOutcome, ReleasePhase, mock_analysis, mock_no_mutations,
    mock_strict_finalize, options, publisher, write_fixture_repo,
};

/// Mutating endpoints the bump phase must not reach: it stops once the version
/// commit is on the branch, leaving the version untagged.
fn mock_no_tagging(server: &mut ServerGuard) -> Vec<Mock> {
    vec![
        server.mock("POST", "/repos/acme/demo/git/tags").expect(0).create(),
        server.mock("POST", "/repos/acme/demo/git/refs").expect(0).create(),
        server.mock("POST", "/repos/acme/demo/releases").expect(0).create(),
    ]
}

fn mock_commit_chain(server: &mut ServerGuard) -> Vec<Mock> {
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

#[test]
fn bump_phase_commits_the_version_without_tagging_it() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _chain = mock_commit_chain(&mut server);
    let _no_tagging = mock_no_tagging(&mut server);
    let advance = server
        .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
        .match_body(Matcher::Regex(r#""sha":"newcommitsha""#.into()))
        .expect(1)
        .with_status(200)
        .with_body(r#"{"ref":"refs/heads/main"}"#)
        .create();

    let mut release_options = options(temp_dir.path());
    release_options.phase = ReleasePhase::Bump;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::VersionCommitted);
    assert_eq!(report.next_version.as_deref(), Some("0.3.0"));
    assert_eq!(report.commit_sha.as_deref(), Some("newcommitsha"));
    assert_eq!(report.release_url, None);
    advance.assert();
}

#[test]
fn tag_phase_releases_the_manifest_version_without_bumping_it() {
    let temp_dir = write_fixture_repo();
    fs::write(temp_dir.path().join("runtime.Dockerfile"), "FROM example@sha256:abc\n")
        .expect("write runtime pin");
    let mut server = Server::new();
    // v0.2.3 is the latest tag and the manifest still reads 0.2.3, so the tag
    // phase must release 0.2.3 rather than computing 0.3.0 from the commits.
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _tag_lookup = server
        .mock("GET", "/repos/acme/demo/git/ref/tags/v0.2.3")
        .with_status(404)
        .with_body(r#"{"message":"Not Found"}"#)
        .create();
    let _chain = mock_commit_chain(&mut server);
    let _advance = server
        .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
        .with_status(200)
        .with_body(r#"{"ref":"refs/heads/main"}"#)
        .create();
    let manifest_blob = server
        .mock("POST", "/repos/acme/demo/git/blobs")
        .match_body(Matcher::Regex(r"version = ".into()))
        .expect(0)
        .create();
    let tag_object = server
        .mock("POST", "/repos/acme/demo/git/tags")
        .match_body(Matcher::Regex(r#""tag":"v0\.2\.3""#.into()))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"sha":"tagobjectsha"}"#)
        .create();
    let _tag_ref = server
        .mock("POST", "/repos/acme/demo/git/refs")
        .with_status(201)
        .with_body(r#"{"ref":"refs/tags/v0.2.3"}"#)
        .create();
    let _release = server
        .mock("POST", "/repos/acme/demo/releases")
        .match_body(Matcher::Regex(r#""tag_name":"v0\.2\.3""#.into()))
        .with_status(201)
        .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.2.3"}"#)
        .create();

    let mut release_options = options(temp_dir.path());
    release_options.phase = ReleasePhase::Tag;
    release_options.extra_files = vec![PathBuf::from("runtime.Dockerfile")];
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.next_version.as_deref(), Some("0.2.3"));
    assert_eq!(report.tag.as_deref(), Some("v0.2.3"));
    // Only the pin rides along; the manifest is already at the released version.
    assert_eq!(report.files_updated.len(), 1);
    assert!(report.files_updated[0].ends_with("runtime.Dockerfile"));
    manifest_blob.assert();
    tag_object.assert();
}

#[test]
fn tag_phase_tags_the_existing_head_when_nothing_is_staged() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _tag_lookup = server
        .mock("GET", "/repos/acme/demo/git/ref/tags/v0.2.3")
        .with_status(404)
        .with_body(r#"{"message":"Not Found"}"#)
        .create();
    // No staged files means no commit and no branch update: an empty commit
    // would only add noise, so the tag lands on the head that was analyzed.
    let no_commit = server.mock("POST", "/repos/acme/demo/git/commits").expect(0).create();
    let no_advance =
        server.mock("PATCH", "/repos/acme/demo/git/refs/heads/main").expect(0).create();
    let tag_object = server
        .mock("POST", "/repos/acme/demo/git/tags")
        .match_body(Matcher::Regex(r#""object":"basecommitsha""#.into()))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"sha":"tagobjectsha"}"#)
        .create();
    let _tag_ref = server
        .mock("POST", "/repos/acme/demo/git/refs")
        .with_status(201)
        .with_body(r#"{"ref":"refs/tags/v0.2.3"}"#)
        .create();
    let _release = server
        .mock("POST", "/repos/acme/demo/releases")
        .match_body(Matcher::Regex(r#""target_commitish":"basecommitsha""#.into()))
        .with_status(201)
        .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.2.3"}"#)
        .create();

    let mut release_options = options(temp_dir.path());
    release_options.phase = ReleasePhase::Tag;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.commit_sha.as_deref(), Some("basecommitsha"));
    assert!(report.files_updated.is_empty());
    no_commit.assert();
    no_advance.assert();
    tag_object.assert();
}

#[test]
fn tag_phase_skips_when_the_manifest_version_is_already_tagged() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 1, FEAT_AND_FIX);
    let _no_mutations = mock_no_mutations(&mut server);
    let _tag_lookup = server
        .mock("GET", "/repos/acme/demo/git/ref/tags/v0.2.3")
        .with_status(200)
        .with_body(r#"{"ref":"refs/tags/v0.2.3","object":{"sha":"tagsha"}}"#)
        .create();

    let mut release_options = options(temp_dir.path());
    release_options.phase = ReleasePhase::Tag;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedTagExists);
}

#[test]
fn tag_phase_releases_even_when_no_commit_warrants_a_bump() {
    // The bump already happened in the earlier phase, so the release must not
    // depend on the commit range still containing a feat or fix.
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, super::CHORE_ONLY);
    let _tag_lookup = server
        .mock("GET", "/repos/acme/demo/git/ref/tags/v0.2.3")
        .with_status(404)
        .with_body(r#"{"message":"Not Found"}"#)
        .create();
    let _finalize = server
        .mock("POST", "/repos/acme/demo/git/tags")
        .with_status(201)
        .with_body(r#"{"sha":"tagobjectsha"}"#)
        .create();
    let _tag_ref = server
        .mock("POST", "/repos/acme/demo/git/refs")
        .with_status(201)
        .with_body(r#"{"ref":"refs/tags/v0.2.3"}"#)
        .create();
    let _release = server
        .mock("POST", "/repos/acme/demo/releases")
        .with_status(201)
        .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.2.3"}"#)
        .create();

    let mut release_options = options(temp_dir.path());
    release_options.phase = ReleasePhase::Tag;
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.bump, None);
}

#[test]
fn phases_cannot_be_combined_with_the_release_pull_request_flow() {
    let temp_dir = write_fixture_repo();
    let server = Server::new();

    let mut release_options = options(temp_dir.path());
    release_options.create_pr = true;
    release_options.phase = ReleasePhase::Bump;
    let error = publisher(&server).release(&release_options).expect_err("combination must fail");

    assert!(error.to_string().contains("--phase"), "unexpected error: {error}");
}

#[test]
fn bump_phase_outputs_the_commit_for_the_follow_up_phase() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _chain = mock_commit_chain(&mut server);
    let _advance = server
        .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
        .with_status(200)
        .with_body(r#"{"ref":"refs/heads/main"}"#)
        .create();
    let _no_tagging = mock_no_tagging(&mut server);

    let mut release_options = options(temp_dir.path());
    release_options.phase = ReleasePhase::Bump;
    let report = publisher(&server).release(&release_options).expect("release report");

    let outputs = report.github_outputs(std::path::Path::new("release_notes.md"));
    assert!(outputs.contains("released=false\n"), "unexpected outputs: {outputs}");
    assert!(outputs.contains("version=0.3.0\n"), "unexpected outputs: {outputs}");
    assert!(outputs.contains("commit=newcommitsha\n"), "unexpected outputs: {outputs}");
}

#[test]
fn strict_finalize_mocks_stay_shared_with_the_single_run_flow() {
    // Guards the assumption the other phase tests rely on: the default phase
    // still bumps, tags, and releases in one run.
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _chain = mock_commit_chain(&mut server);
    let _advance = server
        .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
        .with_status(200)
        .with_body(r#"{"ref":"refs/heads/main"}"#)
        .create();
    let _finalize = mock_strict_finalize(&mut server);

    let report = publisher(&server).release(&options(temp_dir.path())).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.tag.as_deref(), Some("v0.3.0"));
}
