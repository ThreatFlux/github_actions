use std::{collections::BTreeMap, fs};

use mockito::Server;
use tempfile::tempdir;

use super::GitHubClient;

#[test]
fn tracked_source_tree_excludes_symlinks_submodules_and_directories() {
    let mut server = Server::new();
    let request = server
        .mock("GET", "/repos/acme/demo/git/trees/exacthead?recursive=1")
        .with_status(200)
        .with_body(
            r#"{"truncated":false,"tree":[
            {"path":"Cargo.toml","mode":"100644","type":"blob","sha":"regular"},
            {"path":"script","mode":"100755","type":"blob","sha":"executable"},
            {"path":"fuzz/Cargo.lock","mode":"120000","type":"blob","sha":"symlink"},
            {"path":"vendor","mode":"160000","type":"commit","sha":"submodule"},
            {"path":"fuzz","mode":"040000","type":"tree","sha":"directory"}
        ]}"#,
        )
        .create();
    let client = GitHubClient::new(server.url(), None).expect("client");
    let files = client.regular_files_at_commit("acme", "demo", "exacthead").expect("source tree");
    assert_eq!(
        files,
        BTreeMap::from([
            ("Cargo.toml".to_owned(), "regular".to_owned()),
            ("script".to_owned(), "executable".to_owned()),
        ])
    );
    request.assert();
    drop(server);
}

#[test]
fn untracked_source_is_rejected_without_a_blob_request() {
    let repo = tempdir().expect("repository");
    let root = repo.path().canonicalize().expect("canonical repository");
    let file = root.join("Cargo.toml");
    fs::write(&file, "ambient").expect("local source");
    let mut server = Server::new();
    let no_blob = server.mock("GET", mockito::Matcher::Any).expect(0).create();
    let client = GitHubClient::new(server.url(), None).expect("client");
    let error = client
        .validate_release_sources(
            "acme",
            "demo",
            &root,
            &BTreeMap::new(),
            &BTreeMap::from([(file, "ambient".to_owned())]),
        )
        .expect_err("untracked source rejected");
    assert!(error.to_string().contains("not a tracked regular file"));
    no_blob.assert();
    drop(server);
}
