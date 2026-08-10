use std::{fs, path::PathBuf};

use mockito::{Matcher, Mock, Server, ServerGuard};
use tempfile::TempDir;

use super::{
    FEAT_AND_FIX, mock_analysis, mock_no_mutations, mock_strict_finalize, mock_strict_pipeline,
    options, publisher, write_fixture_repo,
};
use crate::release::ReleaseOutcome;

const RUNTIME_PIN: &str = "FROM example@sha256:abc\n";

fn fixture_repo_with_runtime_pin() -> TempDir {
    let temp_dir = write_fixture_repo();
    fs::write(temp_dir.path().join("runtime.Dockerfile"), RUNTIME_PIN).expect("write runtime pin");
    temp_dir
}

/// Blob mocks that pin each staged path to its own SHA, so the tree assertion
/// can prove the extra file was staged as a distinct entry.
fn mock_split_blobs(server: &mut ServerGuard) -> (Mock, Mock) {
    let manifest_blob = server
        .mock("POST", "/repos/acme/demo/git/blobs")
        .match_body(Matcher::Regex(r#"version = \\"0\.3\.0\\""#.into()))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"sha":"manifestblobsha"}"#)
        .create();
    let extra_blob = server
        .mock("POST", "/repos/acme/demo/git/blobs")
        .match_body(Matcher::Regex(r"FROM example@sha256:abc".into()))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"sha":"extrablobsha"}"#)
        .create();
    (manifest_blob, extra_blob)
}

fn mock_tree_with_extra_entry(server: &mut ServerGuard) -> Mock {
    server
        .mock("POST", "/repos/acme/demo/git/trees")
        .match_body(Matcher::AllOf(vec![
            Matcher::Regex(r#""path":"Cargo\.toml""#.into()),
            Matcher::Regex(r#""path":"runtime\.Dockerfile""#.into()),
            Matcher::Regex(r#""sha":"extrablobsha""#.into()),
        ]))
        .expect(1)
        .with_status(201)
        .with_body(r#"{"sha":"treesha"}"#)
        .create()
}

fn mock_base_commit_and_advance(server: &mut ServerGuard) -> Vec<Mock> {
    vec![
        server
            .mock("GET", "/repos/acme/demo/git/commits/basecommitsha")
            .with_status(200)
            .with_body(r#"{"sha":"basecommitsha","tree":{"sha":"basetreesha"}}"#)
            .create(),
        server
            .mock("POST", "/repos/acme/demo/git/commits")
            .with_status(201)
            .with_body(r#"{"sha":"newcommitsha"}"#)
            .create(),
        server
            .mock("PATCH", "/repos/acme/demo/git/refs/heads/main")
            .with_status(200)
            .with_body(r#"{"ref":"refs/heads/main"}"#)
            .create(),
    ]
}

#[test]
fn release_stages_extra_files_into_the_release_commit() {
    let temp_dir = fixture_repo_with_runtime_pin();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _chain = mock_base_commit_and_advance(&mut server);
    let (manifest_blob, extra_blob) = mock_split_blobs(&mut server);
    let tree = mock_tree_with_extra_entry(&mut server);
    let _finalize = mock_strict_finalize(&mut server);

    let mut release_options = options(temp_dir.path());
    release_options.extra_files = vec![PathBuf::from("runtime.Dockerfile")];
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.files_updated.len(), 2);
    assert!(
        report.files_updated.iter().any(|file| file.ends_with("runtime.Dockerfile")),
        "expected the extra file to be staged: {:?}",
        report.files_updated
    );
    manifest_blob.assert();
    extra_blob.assert();
    tree.assert();
}

#[test]
fn release_ignores_extra_files_the_version_rewrite_already_covers() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 2, FEAT_AND_FIX);
    let _pipeline = mock_strict_pipeline(&mut server);
    let _finalize = mock_strict_finalize(&mut server);

    let mut release_options = options(temp_dir.path());
    // Duplicated on purpose: the manifest rewrite carries the bumped version,
    // so staging the pre-bump working-tree copy would revert the release.
    release_options.extra_files = vec![PathBuf::from("Cargo.toml"), PathBuf::from("./Cargo.toml")];
    let report = publisher(&server).release(&release_options).expect("release report");

    assert_eq!(report.outcome, ReleaseOutcome::Released);
    assert_eq!(report.files_updated.len(), 1);
}

#[test]
fn release_fails_when_an_extra_file_is_missing() {
    let temp_dir = write_fixture_repo();
    let mut server = Server::new();
    let _analysis = mock_analysis(&mut server, 1, FEAT_AND_FIX);
    let _no_mutations = mock_no_mutations(&mut server);

    let mut release_options = options(temp_dir.path());
    release_options.extra_files = vec![PathBuf::from("missing.Dockerfile")];
    let error = publisher(&server).release(&release_options).expect_err("missing file must fail");

    assert!(error.to_string().contains("missing.Dockerfile"), "unexpected error: {error}");
}
