use super::{fixture, updated, write};
use crate::versioning::plan_version_rewrite_in_tree;

const SIBLING: &str = "[package]\nname = \"demo-fuzz\"\nversion = \"0.0.0\"\nworkspace = \"../fuzz\"\n[dev-dependencies]\ndemo = { path = \"..\", version = \"=0.2.3\" }\n";

#[test]
fn declared_sibling_workspace_member_updates_its_dev_requirement_and_nested_lock() {
    let (repo, mut tracked) = fixture("");
    write(repo.path(), "fuzz/Cargo.toml", "[workspace]\nmembers = [\"../tools\"]\n");
    write(repo.path(), "tools/Cargo.toml", SIBLING);
    tracked.insert("tools/Cargo.toml".to_owned());
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked)
        .expect("explicit sibling member");
    let member = updated(&rewrite, "tools/Cargo.toml");
    assert!(member.contains("version = \"0.0.0\""));
    assert!(member.contains("path = \"..\", version = \"0.3.0\""));
    assert!(updated(&rewrite, "fuzz/Cargo.lock").contains("name = \"demo\"\nversion = \"0.3.0\""));
    assert!(rewrite.original_files.keys().any(|file| file.ends_with("tools/Cargo.toml")));
}

#[test]
fn sibling_member_globs_honor_workspace_excludes() {
    let (repo, mut tracked) = fixture("");
    write(
        repo.path(),
        "fuzz/Cargo.toml",
        "[workspace]\nmembers = [\"../tools-*\"]\nexclude = [\"../tools-archive\"]\n",
    );
    for directory in ["tools-active", "tools-archive", "unrelated"] {
        let manifest = format!("{directory}/Cargo.toml");
        write(repo.path(), &manifest, SIBLING);
        tracked.insert(manifest);
    }
    let rewrite =
        plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked).expect("sibling glob member");
    assert!(updated(&rewrite, "tools-active/Cargo.toml").contains("version = \"0.3.0\""));
    for directory in ["tools-archive", "unrelated"] {
        assert!(
            rewrite
                .plan
                .file_updates
                .iter()
                .all(|update| !update.file.ends_with(format!("{directory}/Cargo.toml")))
        );
    }
}

#[test]
fn untracked_or_outside_repository_members_do_not_enter_the_release_plan() {
    let (repo, tracked) = fixture("");
    let outside = tempfile::tempdir().expect("outside source");
    let member = outside.path().to_string_lossy().replace('\\', "/");
    write(
        repo.path(),
        "fuzz/Cargo.toml",
        &format!("[workspace]\nmembers = [\"../tools\", {member:?}]\n"),
    );
    write(repo.path(), "tools/Cargo.toml", SIBLING);
    write(outside.path(), "Cargo.toml", SIBLING);
    let rewrite = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked)
        .expect("non-source members excluded");
    assert!(
        rewrite.plan.file_updates.iter().all(|update| !update.file.ends_with("fuzz/Cargo.lock"))
    );
    assert!(
        rewrite
            .original_files
            .keys()
            .all(|file| !file.ends_with("tools/Cargo.toml") && !file.starts_with(outside.path()))
    );
}

#[cfg(unix)]
#[test]
fn tracked_symlink_sibling_member_fails_closed() {
    let (repo, mut tracked) = fixture("");
    write(repo.path(), "fuzz/Cargo.toml", "[workspace]\nmembers = [\"../tools\"]\n");
    write(repo.path(), "archive/Cargo.toml", SIBLING);
    std::fs::create_dir(repo.path().join("tools")).expect("member directory");
    std::os::unix::fs::symlink("../archive/Cargo.toml", repo.path().join("tools/Cargo.toml"))
        .expect("redirected member");
    tracked.insert("tools/Cargo.toml".to_owned());
    let Err(error) = plan_version_rewrite_in_tree(repo.path(), "0.3.0", &tracked) else {
        panic!("symlink member must not become a release source");
    };
    assert!(error.to_string().contains("workspace member"));
    assert!(error.to_string().contains("missing or non-regular"));
}
