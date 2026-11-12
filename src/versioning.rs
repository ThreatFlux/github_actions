//! Release version rewriting across Cargo manifests and lockfiles.
//!
//! Ports the semantics of `scripts/release_version.py` onto `toml_edit`:
//! read the current version from `[package]` (falling back to
//! `[workspace.package]`), then rewrite the version in every workspace member
//! manifest — including internal dependency entries that pin a member's
//! version — plus the workspace `Cargo.lock`, without shelling out to cargo.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Value, value};

use crate::{cargo, model::FileUpdate};

#[path = "versioning_nested.rs"]
mod nested;
pub(crate) use nested::{
    plan_read_only_rewrite, plan_version_rewrite_in_tree, tracked_nested_lockfiles,
};

pub(crate) type SourceFiles = BTreeMap<PathBuf, String>;

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
    plan_with_sources(repo_root, new_version).map(|(plan, _)| plan)
}

fn plan_with_sources(
    repo_root: &Path,
    new_version: &str,
) -> Result<(VersionRewritePlan, SourceFiles)> {
    let mut sources = SourceFiles::new();
    let repo_root = repo_root
        .canonicalize()
        .with_context(|| format!("failed to resolve repository root '{}'", repo_root.display()))?;
    let root_manifest = repo_root.join("Cargo.toml");
    let root_document = source_document(&root_manifest, &mut sources)?;
    let current_version = version_from_document(&root_document).ok_or_else(|| {
        anyhow::anyhow!("unable to determine current version from '{}'", root_manifest.display())
    })?;

    let manifests = member_manifests(&repo_root, &root_document);
    let internal_packages = internal_package_names(&manifests, &mut sources)?;
    let mut file_updates =
        plan_manifest_updates(&manifests, &internal_packages, new_version, &mut sources)?;

    if let Some(lockfile_update) = plan_lockfile_update(
        &repo_root.join("Cargo.lock"),
        &internal_packages,
        None,
        new_version,
        &mut sources,
    )? {
        file_updates.push(lockfile_update);
    }

    if file_updates.is_empty() {
        bail!("no Cargo.toml files were updated");
    }

    Ok((
        VersionRewritePlan {
            current_version,
            internal_packages: internal_packages.into_iter().collect(),
            file_updates,
        },
        sources,
    ))
}

fn plan_manifest_updates(
    manifests: &[PathBuf],
    internal_packages: &BTreeSet<String>,
    new_version: &str,
    sources: &mut SourceFiles,
) -> Result<Vec<FileUpdate>> {
    let mut file_updates = Vec::new();
    for manifest in manifests {
        let original = source_text(manifest, sources)?;
        let mut document = original
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse Cargo manifest '{}'", manifest.display()))?;
        if rewrite_document(&mut document, internal_packages, new_version) {
            let updated_content = document.to_string();
            if updated_content != original {
                file_updates.push(FileUpdate { file: manifest.clone(), updated_content });
            }
        }
    }
    Ok(file_updates)
}

fn source_text(file: &Path, sources: &mut SourceFiles) -> Result<String> {
    if let Some(original) = sources.get(file) {
        return Ok(original.clone());
    }
    let original = fs::read_to_string(file)
        .with_context(|| format!("failed to read release source '{}'", file.display()))?;
    sources.insert(file.to_path_buf(), original.clone());
    Ok(original)
}

fn source_document(file: &Path, sources: &mut SourceFiles) -> Result<DocumentMut> {
    source_text(file, sources)?
        .parse::<DocumentMut>()
        .with_context(|| format!("failed to parse release source '{}'", file.display()))
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
    member_manifests_from_paths(repo_root, root_document, cargo::discover_manifest_files(repo_root))
}

fn member_manifests_from_paths(
    repo_root: &Path,
    root_document: &DocumentMut,
    candidates: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    let root_manifest = repo_root.join("Cargo.toml");
    let mut manifests = vec![root_manifest.clone()];

    let patterns = workspace_path_list(root_document, "members");
    if patterns.is_empty() {
        return manifests;
    }
    let excludes = workspace_path_list(root_document, "exclude");

    for manifest in candidates {
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
        if patterns.iter().any(|pattern| matches_member_pattern(pattern, &relative))
            && !excludes.iter().any(|pattern| matches_member_pattern(pattern, &relative))
        {
            manifests.push(manifest);
        }
    }

    manifests
}

fn workspace_path_list(root_document: &DocumentMut, key: &str) -> Vec<String> {
    root_document
        .get("workspace")
        .and_then(Item::as_table_like)
        .and_then(|workspace| workspace.get(key))
        .and_then(Item::as_array)
        .map(|entries| entries.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect())
        .unwrap_or_default()
}

fn internal_package_names(
    manifests: &[PathBuf],
    sources: &mut SourceFiles,
) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for manifest in manifests {
        let document = source_document(manifest, sources)?;
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
    lockfile: &Path,
    internal_packages: &BTreeSet<String>,
    current_versions: Option<&BTreeMap<String, String>>,
    new_version: &str,
    sources: &mut SourceFiles,
) -> Result<Option<FileUpdate>> {
    if !lockfile.is_file() {
        return Ok(None);
    }

    let original = source_text(lockfile, sources)?;
    let mut document = original
        .parse::<DocumentMut>()
        .with_context(|| format!("failed to parse lockfile '{}'", lockfile.display()))?;
    if let Some(versions) = current_versions {
        validate_owned_lock_versions(&document, versions, lockfile)?;
    }
    let mut changed = false;
    let mut rewritten = BTreeSet::new();

    if let Some(packages) = document.get_mut("package").and_then(Item::as_array_of_tables_mut) {
        for package in packages.iter_mut() {
            // A `source` marks a registry/git entry, which can share a
            // workspace member's name (for example a dev-dependency on an
            // older published version of the same crate).
            let name = package.get("name").and_then(Item::as_str).unwrap_or_default();
            let old_version = package.get("version").and_then(Item::as_str).unwrap_or_default();
            let matches_current = current_versions.is_none_or(|versions| {
                versions.get(name).is_some_and(|current| current == old_version)
            });
            if !internal_packages.contains(name)
                || package.get("source").is_some()
                || !matches_current
            {
                continue;
            }
            rewritten.insert((name.to_owned(), old_version.to_owned()));
            if let Some(version_item) = package.get_mut("version")
                && version_item.as_str().is_some()
            {
                *version_item = value(new_version);
                changed = true;
            }
        }
        rewrite_lockfile_references(packages, &rewritten, new_version);
    }

    Ok(changed.then(|| FileUpdate {
        file: lockfile.to_path_buf(),
        updated_content: document.to_string(),
    }))
}

fn validate_owned_lock_versions(
    document: &DocumentMut,
    versions: &BTreeMap<String, String>,
    lockfile: &Path,
) -> Result<()> {
    let Some(packages) = document.get("package").and_then(Item::as_array_of_tables) else {
        return Ok(());
    };
    for (name, current) in versions {
        let locked: BTreeSet<_> = packages
            .iter()
            .filter(|package| {
                package.get("source").is_none()
                    && package.get("name").and_then(Item::as_str) == Some(name.as_str())
            })
            .filter_map(|package| package.get("version").and_then(Item::as_str))
            .collect();
        if !locked.is_empty() && !locked.contains(current.as_str()) {
            bail!(
                "nested lockfile '{}' has stale or ambiguous owned path package '{name}': expected {current}; refresh this consumer lock before releasing",
                lockfile.display()
            );
        }
    }
    Ok(())
}

fn rewrite_lockfile_references(
    packages: &mut toml_edit::ArrayOfTables,
    rewritten: &BTreeSet<(String, String)>,
    new_version: &str,
) {
    for package in packages.iter_mut() {
        let Some(dependencies) = package.get_mut("dependencies").and_then(Item::as_array_mut)
        else {
            continue;
        };
        for dependency in dependencies.iter_mut() {
            let Some(reference) = dependency.as_str() else {
                continue;
            };
            let fields: Vec<_> = reference.split_whitespace().collect();
            // Registry/git-qualified references have a third source component.
            if fields.len() == 2
                && rewritten.contains(&(fields[0].to_owned(), fields[1].to_owned()))
            {
                let decor = dependency.decor().clone();
                *dependency = Value::from(format!("{} {new_version}", fields[0]));
                *dependency.decor_mut() = decor;
            }
        }
    }
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

// Tests live in a sibling file to keep this module within the repository's
// file-size lint budget; they remain `super::`-scoped unit tests.
#[cfg(test)]
#[path = "versioning_tests.rs"]
mod tests;
