//! Bind automated publication to the exact head accepted by its validation gate.

use std::fs;

use mockito::{Matcher, Mock, Server, ServerGuard};

use super::{
    FEAT_AND_FIX, ReleaseOptions, ReleaseOutcome, ReleasePhase, TAGS_V023, mock_analysis,
    mock_build_chain, mock_compare, mock_head_ref, mock_strict_finalize, mock_strict_pipeline,
    mock_tag_lookup, mock_tags, options, publisher, write_fixture_repo,
};

/// A mismatched validation head must stop before history reads or any write.
fn mock_nothing_after_head(server: &mut ServerGuard) -> Vec<Mock> {
    let mut mocks = vec![
        server
            .mock(
                "GET",
                Matcher::Regex(r"^/repos/acme/demo/(tags|compare|commits|git/ref/tags/)".into()),
            )
            .expect(0)
            .create(),
    ];
    for method in ["POST", "PATCH", "PUT", "DELETE"] {
        mocks.push(server.mock(method, Matcher::Any).expect(0).create());
    }
    mocks
}

fn assert_mismatched_head_refused(configure: impl FnOnce(&mut ReleaseOptions)) {
    let temp_dir = write_fixture_repo();
    // The head guard must precede even the local manifest read, not merely
    // refuse mutations after planning a release from the wrong checkout.
    let manifest = temp_dir.path().join("Cargo.toml");
    fs::write(&manifest, "not a valid Cargo manifest").expect("replace manifest");
    let mut server = Server::new();
    let head = mock_head_ref(&mut server, "advancedsha", 1);
    let nothing_after = mock_nothing_after_head(&mut server);
    let mut release_options = options(temp_dir.path());
    configure(&mut release_options);

    let error = publisher(&server)
        .release_at_head(&release_options, "validatedsha")
        .expect_err("unvalidated branch head must not release");
    let message = error.to_string();
    assert!(message.contains("release branch 'main' head advancedsha"), "{message}");
    assert!(message.contains("does not match expected head validatedsha"), "{message}");
    assert_eq!(fs::read_to_string(manifest).expect("read manifest"), "not a valid Cargo manifest");
    head.assert();
    for mock in &nothing_after {
        mock.assert();
    }
}

#[test]
fn mismatched_head_refuses_every_release_phase_before_analysis() {
    for phase in [ReleasePhase::All, ReleasePhase::Bump, ReleasePhase::Tag] {
        assert_mismatched_head_refused(|options| options.phase = phase);
    }
}

#[test]
fn mismatched_head_also_refuses_dry_run_and_release_pull_request() {
    assert_mismatched_head_refused(|options| options.dry_run = true);
    assert_mismatched_head_refused(|options| options.create_pr = true);
}

#[test]
fn empty_expected_head_cannot_silently_disable_the_library_guard() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let no_request = server.mock("GET", Matcher::Any).expect(0).create();

    for expected in ["", " "] {
        let error = publisher(&server)
            .release_at_head(&options(temp_dir.path()), expected)
            .expect_err("explicit empty guard must fail");
        assert!(error.to_string().contains("expected release head must not be empty"));
    }
    no_request.assert();
}

#[test]
fn matching_head_allows_the_normal_commit_tag_and_release() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let pipeline = mock_strict_pipeline(&mut server);
    let finalize = mock_strict_finalize(&mut server);

    let report = publisher(&server)
        .release_at_head(&options(temp_dir.path()), "basecommitsha")
        .expect("validated release");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.commit_sha.as_deref(), Some("newcommitsha"));
    for mock in analysis.iter().chain(&pipeline).chain(&finalize) {
        mock.assert();
    }
}

#[test]
fn matching_head_tag_phase_tags_the_validated_manifest_head() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let head = mock_head_ref(&mut server, "basecommitsha", 2);
    let tags = mock_tags(&mut server, TAGS_V023);
    let compare = mock_compare(&mut server, FEAT_AND_FIX);
    let tag_lookup = mock_tag_lookup(&mut server, "v0.2.3", 404, r#"{"message":"Not Found"}"#);
    let tag = server
        .mock("POST", "/repos/acme/demo/git/tags")
        .match_body(Matcher::Regex(r#""object":"basecommitsha""#.into()))
        .with_status(201)
        .with_body(r#"{"sha":"tagobjectsha"}"#)
        .create();
    let reference = server
        .mock("POST", "/repos/acme/demo/git/refs")
        .with_status(201)
        .with_body(r#"{"ref":"refs/tags/v0.2.3"}"#)
        .create();
    let release = server
        .mock("POST", "/repos/acme/demo/releases")
        .with_status(201)
        .with_body(r#"{"html_url":"https://github.com/acme/demo/releases/tag/v0.2.3"}"#)
        .create();
    let mut release_options = options(temp_dir.path());
    release_options.phase = ReleasePhase::Tag;

    let report = publisher(&server)
        .release_at_head(&release_options, "basecommitsha")
        .expect("validated tag release");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.commit_sha.as_deref(), Some("basecommitsha"));
    for mock in [head, tags, compare, tag_lookup, tag, reference, release] {
        mock.assert();
    }
}

#[test]
fn matching_initial_head_retains_the_mid_run_race_check() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let initial_head = mock_head_ref(&mut server, "basecommitsha", 1);
    let moved_head = mock_head_ref(&mut server, "advancedsha", 1);
    let tags = mock_tags(&mut server, TAGS_V023);
    let compare = mock_compare(&mut server, FEAT_AND_FIX);
    let tag_lookup = mock_tag_lookup(&mut server, "v0.3.0", 404, r#"{"message":"Not Found"}"#);
    let build = mock_build_chain(&mut server);
    let no_advance =
        server.mock("PATCH", "/repos/acme/demo/git/refs/heads/main").expect(0).create();
    let no_tag = server.mock("POST", "/repos/acme/demo/git/tags").expect(0).create();
    let no_release = server.mock("POST", "/repos/acme/demo/releases").expect(0).create();

    let report = publisher(&server)
        .release_at_head(&options(temp_dir.path()), "basecommitsha")
        .expect("race report");

    assert_eq!(report.outcome, ReleaseOutcome::SkippedRace);
    assert_eq!(report.commit_sha, None);
    for mock in
        [initial_head, moved_head, tags, compare, tag_lookup, no_advance, no_tag, no_release]
    {
        mock.assert();
    }
    for mock in &build {
        mock.assert();
    }
}
