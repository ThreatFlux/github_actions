//! Release version rewriting across Cargo manifests and lockfiles.
//!
//! Ports the semantics of `scripts/release_version.py` onto `toml_edit`:
//! read the current version from `[package]` (falling back to
//! `[workspace.package]`), then rewrite the version in every workspace member
//! manifest — including internal dependency entries that pin a member's
//! version — plus the workspace `Cargo.lock`, without shelling out to cargo.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Value, value};

use crate::{cargo, model::FileUpdate};

const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct VersionRewritePlan {
    pub current_version: String,
    pub internal_packages: Vec<String>,
    pub file_updates: Vec<FileUpdate>,
}

/// Read the release version from the repository root manifest.
pub fn current_version(repo_root: &Path) -> Result<String> {
    let manifest = repo_root.join("Cargo.toml");
    let document = parse_manifest(&manifest)?;
    version_from_document(&document).ok_or_else(|| {
        anyhow::anyhow!("unable to determine current version from '{}'", manifest.display())
    })
}

/// Plan the rewrite of every member manifest and the lockfile to
/// `new_version`, without touching the filesystem.
pub fn plan_version_rewrite(repo_root: &Path, new_version: &str) -> Result<VersionRewritePlan> {
    let repo_root = repo_root
        .canonicalize()
        .with_context(|| format!("failed to resolve repository root '{}'", repo_root.display()))?;
    let root_manifest = repo_root.join("Cargo.toml");
    let root_document = parse_manifest(&root_manifest)?;
    let current_version = version_from_document(&root_document).ok_or_else(|| {
        anyhow::anyhow!("unable to determine current version from '{}'", root_manifest.display())
    })?;

    let manifests = member_manifests(&repo_root, &root_document);
    let internal_packages = internal_package_names(&manifests)?;
    let mut file_updates = Vec::new();

    for manifest in &manifests {
        let original = fs::read_to_string(manifest)
            .with_context(|| format!("failed to read Cargo manifest '{}'", manifest.display()))?;
        let mut document = original
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse Cargo manifest '{}'", manifest.display()))?;
        if rewrite_document(&mut document, &internal_packages, new_version) {
            let updated_content = document.to_string();
            if updated_content != original {
                file_updates.push(FileUpdate { file: manifest.clone(), updated_content });
            }
        }
    }

    if let Some(lockfile_update) =
        plan_lockfile_update(&repo_root, &internal_packages, new_version)?
    {
        file_updates.push(lockfile_update);
    }

    if file_updates.is_empty() {
        bail!("no Cargo.toml files were updated");
    }

    Ok(VersionRewritePlan {
        current_version,
        internal_packages: internal_packages.into_iter().collect(),
        file_updates,
    })
}

fn parse_manifest(manifest: &Path) -> Result<DocumentMut> {
    let content = fs::read_to_string(manifest)
        .with_context(|| format!("failed to read Cargo manifest '{}'", manifest.display()))?;
    content
        .parse::<DocumentMut>()
        .with_context(|| format!("failed to parse Cargo manifest '{}'", manifest.display()))
}

fn version_from_document(document: &DocumentMut) -> Option<String> {
    let package_version = document
        .get("package")
        .and_then(Item::as_table_like)
        .and_then(|package| package.get("version"))
        .and_then(Item::as_str);
    if let Some(version) = package_version {
        return Some(version.to_owned());
    }

    document
        .get("workspace")
        .and_then(Item::as_table_like)
        .and_then(|workspace| workspace.get("package"))
        .and_then(Item::as_table_like)
        .and_then(|package| package.get("version"))
        .and_then(Item::as_str)
        .map(ToOwned::to_owned)
}

fn member_manifests(repo_root: &Path, root_document: &DocumentMut) -> Vec<PathBuf> {
    let root_manifest = repo_root.join("Cargo.toml");
    let mut manifests = vec![root_manifest.clone()];

    let patterns: Vec<String> = root_document
        .get("workspace")
        .and_then(Item::as_table_like)
        .and_then(|workspace| workspace.get("members"))
        .and_then(Item::as_array)
        .map(|members| members.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect())
        .unwrap_or_default();
    if patterns.is_empty() {
        return manifests;
    }

    for manifest in cargo::discover_manifest_files(repo_root) {
        if manifest == root_manifest {
            continue;
        }
        let Some(parent) = manifest.parent() else {
            continue;
        };
        let Ok(relative) = parent.strip_prefix(repo_root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if patterns.iter().any(|pattern| matches_member_pattern(pattern, &relative)) {
            manifests.push(manifest);
        }
    }

    manifests
}

fn internal_package_names(manifests: &[PathBuf]) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for manifest in manifests {
        let document = parse_manifest(manifest)?;
        if let Some(name) = document
            .get("package")
            .and_then(Item::as_table_like)
            .and_then(|package| package.get("name"))
            .and_then(Item::as_str)
        {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

fn rewrite_document(
    document: &mut DocumentMut,
    internal_packages: &BTreeSet<String>,
    new_version: &str,
) -> bool {
    let mut changed = set_version_string(document, &["package", "version"], new_version);
    changed |= set_version_string(document, &["workspace", "package", "version"], new_version);

    for table_path in dependency_table_paths(document) {
        for name in internal_packages {
            let mut item_path = table_path.clone();
            item_path.push(name.clone());
            if let Some(item) = cargo::get_item_mut(document.as_item_mut(), &item_path) {
                changed |= set_dependency_version(item, new_version);
            }
        }
    }

    changed
}

fn dependency_table_paths(document: &DocumentMut) -> Vec<Vec<String>> {
    let mut paths: Vec<Vec<String>> =
        DEPENDENCY_TABLES.iter().map(|table| vec![(*table).to_owned()]).collect();
    paths.push(vec![String::from("workspace"), String::from("dependencies")]);

    if let Some(target_table) = document.get("target").and_then(Item::as_table) {
        for (target_name, _) in target_table {
            for table in DEPENDENCY_TABLES {
                paths.push(vec![String::from("target"), target_name.to_owned(), table.to_owned()]);
            }
        }
    }

    paths
}

fn set_version_string(document: &mut DocumentMut, path: &[&str], new_version: &str) -> bool {
    let item_path: Vec<String> = path.iter().map(|segment| (*segment).to_owned()).collect();
    if let Some(item) = cargo::get_item_mut(document.as_item_mut(), &item_path)
        && item.as_str().is_some()
    {
        *item = value(new_version);
        return true;
    }
    false
}

fn set_dependency_version(item: &mut Item, new_version: &str) -> bool {
    if let Some(inline_table) = item.as_inline_table_mut() {
        if inline_table.get("version").is_some_and(|version| version.as_str().is_some()) {
            inline_table.insert("version", Value::from(new_version));
            return true;
        }
        return false;
    }

    if let Some(table) = item.as_table_mut() {
        if let Some(version_item) = table.get_mut("version")
            && version_item.as_str().is_some()
        {
            *version_item = value(new_version);
            return true;
        }
        return false;
    }

    if item.as_str().is_some() {
        *item = value(new_version);
        return true;
    }

    false
}

fn plan_lockfile_update(
    repo_root: &Path,
    internal_packages: &BTreeSet<String>,
    new_version: &str,
) -> Result<Option<FileUpdate>> {
    let lockfile = repo_root.join("Cargo.lock");
    if !lockfile.is_file() {
        return Ok(None);
    }

    let original = fs::read_to_string(&lockfile)
        .with_context(|| format!("failed to read lockfile '{}'", lockfile.display()))?;
    let mut document = original
        .parse::<DocumentMut>()
        .with_context(|| format!("failed to parse lockfile '{}'", lockfile.display()))?;
    let mut changed = false;

    if let Some(packages) = document.get_mut("package").and_then(Item::as_array_of_tables_mut) {
        for package in packages.iter_mut() {
            let is_internal = package
                .get("name")
                .and_then(Item::as_str)
                .is_some_and(|name| internal_packages.contains(name));
            if !is_internal {
                continue;
            }
            if let Some(version_item) = package.get_mut("version")
                && version_item.as_str().is_some()
            {
                *version_item = value(new_version);
                changed = true;
            }
        }
    }

    Ok(changed.then(|| FileUpdate { file: lockfile, updated_content: document.to_string() }))
}

/// Match a workspace `members` glob against a `/`-separated relative path.
/// `*` and `?` stay within one path segment; `**` may span several.
fn matches_member_pattern(pattern: &str, path: &str) -> bool {
    let pattern_segments: Vec<&str> =
        pattern.split('/').filter(|segment| !segment.is_empty()).collect();
    let path_segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    match_segments(&pattern_segments, &path_segments)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.first() {
        None => path.is_empty(),
        Some(&"**") => {
            (0..=path.len()).any(|skipped| match_segments(&pattern[1..], &path[skipped..]))
        }
        Some(segment) => {
            !path.is_empty()
                && matches_segment(segment, path[0])
                && match_segments(&pattern[1..], &path[1..])
        }
    }
}

fn matches_segment(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    match_chars(&pattern, &text)
}

fn match_chars(pattern: &[char], text: &[char]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some('*') => (0..=text.len()).any(|skipped| match_chars(&pattern[1..], &text[skipped..])),
        Some('?') => !text.is_empty() && match_chars(&pattern[1..], &text[1..]),
        Some(expected) => text.first() == Some(expected) && match_chars(&pattern[1..], &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{current_version, matches_member_pattern, plan_version_rewrite};

    #[test]
    fn current_version_prefers_package_over_workspace() {
        let temp_dir = tempdir().expect("tempdir");
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.2.3\"\n",
        )
        .expect("write Cargo.toml");

        let version = current_version(temp_dir.path()).expect("current version");

        assert_eq!(version, "0.2.3");
    }

    #[test]
    fn current_version_falls_back_to_workspace_package() {
        let temp_dir = tempdir().expect("tempdir");
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"1.4.0\"\n",
        )
        .expect("write Cargo.toml");

        let version = current_version(temp_dir.path()).expect("current version");

        assert_eq!(version, "1.4.0");
    }

    #[test]
    fn current_version_errors_when_no_version_exists() {
        let temp_dir = tempdir().expect("tempdir");
        fs::write(temp_dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n")
            .expect("write Cargo.toml");

        let error = current_version(temp_dir.path()).expect_err("missing version");

        assert!(error.to_string().contains("unable to determine current version"));
    }

    #[test]
    fn plan_rewrites_single_package_manifest_preserving_formatting() {
        let temp_dir = tempdir().expect("tempdir");
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n\n# release version\nversion = \"0.2.3\"\nedition = \"2024\"\n\n[dependencies]\nanyhow = \"1.0.95\"\n",
        )
        .expect("write Cargo.toml");

        let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

        assert_eq!(plan.current_version, "0.2.3");
        assert_eq!(plan.internal_packages, vec![String::from("demo")]);
        assert_eq!(plan.file_updates.len(), 1);
        let updated = &plan.file_updates[0].updated_content;
        assert!(updated.contains("# release version\nversion = \"0.3.0\""), "{updated}");
        assert!(updated.contains("anyhow = \"1.0.95\""), "{updated}");
    }

    #[test]
    fn plan_rewrites_workspace_members_and_internal_dependencies() {
        let temp_dir = tempdir().expect("tempdir");
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.2.3\"\n\n[workspace.dependencies]\ndemo-core = { path = \"crates/demo-core\", version = \"0.2.3\" }\n",
        )
        .expect("write root Cargo.toml");
        fs::create_dir_all(temp_dir.path().join("crates/demo-core")).expect("create member");
        fs::write(
            temp_dir.path().join("crates/demo-core/Cargo.toml"),
            "[package]\nname = \"demo-core\"\nversion = \"0.2.3\"\n",
        )
        .expect("write member Cargo.toml");
        fs::create_dir_all(temp_dir.path().join("crates/demo-util")).expect("create member");
        fs::write(
            temp_dir.path().join("crates/demo-util/Cargo.toml"),
            "[package]\nname = \"demo-util\"\nversion = \"0.2.3\"\n",
        )
        .expect("write member Cargo.toml");
        fs::create_dir_all(temp_dir.path().join("crates/demo-cli")).expect("create member");
        fs::write(
            temp_dir.path().join("crates/demo-cli/Cargo.toml"),
            "[package]\nname = \"demo-cli\"\nversion = { workspace = true }\n\n[dependencies]\ndemo-core = { path = \"../demo-core\", version = \"0.2.3\" }\nanyhow = \"1.0.95\"\ndemo-extra = { path = \"../../tools/demo-extra\", version = \"0.2.3\" }\n\n[dependencies.demo-util]\npath = \"../demo-util\"\nversion = \"0.2.3\"\n",
        )
        .expect("write member Cargo.toml");
        fs::create_dir_all(temp_dir.path().join("tools/demo-extra")).expect("create non-member");
        fs::write(
            temp_dir.path().join("tools/demo-extra/Cargo.toml"),
            "[package]\nname = \"demo-extra\"\nversion = \"0.2.3\"\n",
        )
        .expect("write non-member Cargo.toml");

        let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

        let updated_files: Vec<String> = plan
            .file_updates
            .iter()
            .map(|update| update.file.to_string_lossy().replace('\\', "/"))
            .collect();
        assert!(updated_files.iter().any(|file| file.ends_with("crates/demo-core/Cargo.toml")));
        assert!(updated_files.iter().any(|file| file.ends_with("crates/demo-cli/Cargo.toml")));
        assert!(
            !updated_files.iter().any(|file| file.ends_with("tools/demo-extra/Cargo.toml")),
            "non-member manifest must not be rewritten"
        );

        let root = plan
            .file_updates
            .iter()
            .find(|update| {
                update.file
                    == temp_dir.path().canonicalize().expect("canonical root").join("Cargo.toml")
            })
            .expect("root update");
        assert!(root.updated_content.contains("version = \"0.3.0\""), "{}", root.updated_content);
        assert!(
            root.updated_content
                .contains("demo-core = { path = \"crates/demo-core\", version = \"0.3.0\" }"),
            "{}",
            root.updated_content
        );

        let cli = plan
            .file_updates
            .iter()
            .find(|update| update.file.ends_with("crates/demo-cli/Cargo.toml"))
            .expect("cli update");
        assert!(
            cli.updated_content
                .contains("demo-core = { path = \"../demo-core\", version = \"0.3.0\" }"),
            "{}",
            cli.updated_content
        );
        assert!(
            cli.updated_content.contains("version = { workspace = true }"),
            "workspace-inherited package version must not be replaced: {}",
            cli.updated_content
        );
        assert!(cli.updated_content.contains("anyhow = \"1.0.95\""), "{}", cli.updated_content);
        // Dependencies on non-member packages keep their pinned version.
        assert!(
            cli.updated_content.contains(
                "demo-extra = { path = \"../../tools/demo-extra\", version = \"0.2.3\" }"
            ),
            "{}",
            cli.updated_content
        );
        // Table-shaped internal dependency ([dependencies.demo-util]).
        assert!(
            cli.updated_content.contains("path = \"../demo-util\"\nversion = \"0.3.0\""),
            "{}",
            cli.updated_content
        );
    }

    #[test]
    fn plan_rewrites_bare_string_internal_dependency() {
        let temp_dir = tempdir().expect("tempdir");
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"lib\"]\n\n[workspace.package]\nversion = \"0.2.3\"\n",
        )
        .expect("write root Cargo.toml");
        fs::create_dir_all(temp_dir.path().join("lib")).expect("create member");
        fs::write(
            temp_dir.path().join("lib/Cargo.toml"),
            "[package]\nname = \"demo-lib\"\nversion = \"0.2.3\"\n\n[dev-dependencies]\ndemo-lib = \"0.2.3\"\n",
        )
        .expect("write member Cargo.toml");

        let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

        let lib = plan
            .file_updates
            .iter()
            .find(|update| update.file.ends_with("lib/Cargo.toml"))
            .expect("lib update");
        assert!(lib.updated_content.contains("demo-lib = \"0.3.0\""), "{}", lib.updated_content);
    }

    #[test]
    fn plan_updates_lockfile_entries_for_internal_packages_only() {
        let temp_dir = tempdir().expect("tempdir");
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.2.3\"\n",
        )
        .expect("write Cargo.toml");
        fs::write(
            temp_dir.path().join("Cargo.lock"),
            "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"anyhow\"\nversion = \"1.0.95\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"abc\"\n\n[[package]]\nname = \"demo\"\nversion = \"0.2.3\"\ndependencies = [\n \"anyhow\",\n]\n",
        )
        .expect("write Cargo.lock");

        let plan = plan_version_rewrite(temp_dir.path(), "0.3.0").expect("rewrite plan");

        let lockfile = plan
            .file_updates
            .iter()
            .find(|update| update.file.ends_with("Cargo.lock"))
            .expect("lockfile update");
        assert!(
            lockfile.updated_content.contains("# This file is automatically @generated by Cargo."),
            "{}",
            lockfile.updated_content
        );
        assert!(lockfile.updated_content.contains("version = 4"), "{}", lockfile.updated_content);
        assert!(
            lockfile.updated_content.contains("name = \"anyhow\"\nversion = \"1.0.95\""),
            "{}",
            lockfile.updated_content
        );
        assert!(
            lockfile.updated_content.contains("name = \"demo\"\nversion = \"0.3.0\""),
            "{}",
            lockfile.updated_content
        );
    }

    #[test]
    fn member_patterns_respect_segment_boundaries() {
        assert!(matches_member_pattern("crates/*", "crates/demo"));
        assert!(!matches_member_pattern("crates/*", "crates/demo/nested"));
        assert!(matches_member_pattern("crates/**", "crates/demo/nested"));
        assert!(matches_member_pattern("**/demo", "a/b/demo"));
        assert!(matches_member_pattern("crates/demo-?", "crates/demo-a"));
        assert!(!matches_member_pattern("crates/demo-?", "crates/demo-ab"));
        assert!(matches_member_pattern("lib", "lib"));
        assert!(!matches_member_pattern("lib", "libs"));
    }
}
