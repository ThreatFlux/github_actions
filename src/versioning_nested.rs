//! Update independently locked consumers only through actual owned path edges.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item};

use super::{
    SourceFiles, VersionRewritePlan, dependency_table_paths, member_manifests,
    plan_lockfile_update, plan_with_sources, set_dependency_version, source_document,
};
use crate::{cargo, model::FileUpdate, remote};

pub struct TreeRewritePlan {
    pub plan: VersionRewritePlan,
    pub original_files: SourceFiles,
}

fn nested_lockfiles(repo_root: &Path) -> Vec<PathBuf> {
    cargo::discover_manifest_files(repo_root)
        .into_iter()
        .filter(|manifest| manifest != &repo_root.join("Cargo.toml"))
        .map(|manifest| manifest.with_file_name("Cargo.lock"))
        .filter(|lockfile| {
            std::fs::symlink_metadata(lockfile).is_ok_and(|metadata| metadata.file_type().is_file())
        })
        .collect()
}

pub fn has_nested_lockfiles(repo_root: &Path) -> bool {
    !nested_lockfiles(repo_root).is_empty()
}

pub fn plan_read_only_rewrite(repo_root: &Path, new_version: &str) -> Result<VersionRewritePlan> {
    let repo_root = repo_root.canonicalize()?;
    let allowed = cargo::discover_manifest_files(&repo_root)
        .into_iter()
        .chain(nested_lockfiles(&repo_root))
        .chain([repo_root.join("Cargo.lock")])
        .filter_map(|file| remote::relative_repository_path(&repo_root, &file).ok())
        .collect();
    Ok(plan_version_rewrite_in_tree(&repo_root, new_version, &allowed)?.plan)
}

pub fn plan_version_rewrite_in_tree(
    repo_root: &Path,
    new_version: &str,
    tracked: &BTreeSet<String>,
) -> Result<TreeRewritePlan> {
    let repo_root = repo_root.canonicalize()?;
    let (mut plan, mut sources) = plan_with_sources(&repo_root, new_version)?;
    let owned = owned_manifests(&repo_root, &mut sources)?;
    let mut updates: BTreeMap<_, _> =
        plan.file_updates.into_iter().map(|update| (update.file, update.updated_content)).collect();
    for lockfile in nested_lockfiles(&repo_root) {
        let manifest = lockfile.with_file_name("Cargo.toml");
        if !is_tracked(&repo_root, &lockfile, tracked)
            || !is_tracked(&repo_root, &manifest, tracked)
        {
            continue;
        }
        let mut context = ConsumerContext::new(&repo_root, tracked, &owned, &sources, &updates);
        if !context.update_consumer(&lockfile, new_version)? {
            continue;
        }
        sources = context.sources;
        updates = context.updates;
    }
    updates.retain(|file, content| sources.get(file) != Some(content));
    plan.file_updates = updates
        .into_iter()
        .map(|(file, updated_content)| FileUpdate { file, updated_content })
        .collect();
    Ok(TreeRewritePlan { plan, original_files: sources })
}

fn owned_manifests(
    repo_root: &Path,
    sources: &mut SourceFiles,
) -> Result<BTreeMap<PathBuf, (String, String)>> {
    let root = source_document(&repo_root.join("Cargo.toml"), sources)?;
    let mut owned = BTreeMap::new();
    for manifest in member_manifests(repo_root, &root) {
        let document = source_document(&manifest, sources)?;
        if let Some(name) = document
            .get("package")
            .and_then(Item::as_table_like)
            .and_then(|package| package.get("name"))
            .and_then(Item::as_str)
        {
            let version = owned_package_version(&document, &root)
                .context("owned package has no release version")?;
            owned.insert(manifest.canonicalize()?, (name.to_owned(), version));
        }
    }
    Ok(owned)
}

fn owned_package_version(document: &DocumentMut, root: &DocumentMut) -> Option<String> {
    let version = document
        .get("package")
        .and_then(Item::as_table_like)
        .and_then(|package| package.get("version"))?;
    if let Some(explicit) = version.as_str() {
        return Some(explicit.to_owned());
    }
    if version.as_table_like()?.get("workspace").and_then(Item::as_bool) != Some(true) {
        return None;
    }
    root.get("workspace")
        .and_then(Item::as_table_like)
        .and_then(|workspace| workspace.get("package"))
        .and_then(Item::as_table_like)
        .and_then(|package| package.get("version"))
        .and_then(Item::as_str)
        .map(ToOwned::to_owned)
}

fn is_tracked(repo_root: &Path, file: &Path, tracked: &BTreeSet<String>) -> bool {
    std::fs::symlink_metadata(file).is_ok_and(|metadata| metadata.file_type().is_file())
        && file
            .canonicalize()
            .is_ok_and(|canonical| canonical == file && canonical.starts_with(repo_root))
        && remote::relative_repository_path(repo_root, file)
            .is_ok_and(|relative| tracked.contains(&relative))
}

struct PathDependency {
    declaration: PathBuf,
    item_path: Vec<String>,
    target: PathBuf,
}

struct ConsumerContext<'a> {
    repo_root: &'a Path,
    tracked: &'a BTreeSet<String>,
    owned: &'a BTreeMap<PathBuf, (String, String)>,
    sources: SourceFiles,
    updates: BTreeMap<PathBuf, String>,
    reached: BTreeMap<String, String>,
    declarations: Vec<PathDependency>,
}

impl<'a> ConsumerContext<'a> {
    fn new(
        repo_root: &'a Path,
        tracked: &'a BTreeSet<String>,
        owned: &'a BTreeMap<PathBuf, (String, String)>,
        sources: &SourceFiles,
        updates: &BTreeMap<PathBuf, String>,
    ) -> Self {
        Self {
            repo_root,
            tracked,
            owned,
            sources: sources.clone(),
            updates: updates.clone(),
            reached: BTreeMap::new(),
            declarations: Vec::new(),
        }
    }

    fn update_consumer(&mut self, lockfile: &Path, new_version: &str) -> Result<bool> {
        let manifest = lockfile.with_file_name("Cargo.toml");
        let document = source_document(&manifest, &mut self.sources)?;
        self.reject_owned_patches(&manifest, &document)?;
        let mut visited = BTreeMap::new();
        for member in member_manifests(manifest.parent().context("consumer root")?, &document) {
            if is_tracked(self.repo_root, &member, self.tracked) {
                self.visit(&member, true, &mut visited)?;
            }
        }
        if self.reached.is_empty() {
            return Ok(false);
        }
        let names = self.reached.keys().cloned().collect();
        if let Some(update) = plan_lockfile_update(
            lockfile,
            &names,
            Some(&self.reached),
            new_version,
            &mut self.sources,
        )? {
            self.updates.insert(update.file, update.updated_content);
        }
        self.rewrite_versions(new_version)?;
        Ok(true)
    }

    fn visit(
        &mut self,
        manifest: &Path,
        include_dev: bool,
        visited: &mut BTreeMap<PathBuf, bool>,
    ) -> Result<()> {
        if visited.get(manifest).is_some_and(|previous| *previous || !include_dev) {
            return Ok(());
        }
        visited.insert(manifest.to_path_buf(), include_dev);
        let document = source_document(manifest, &mut self.sources)?;
        for dependency in self.path_dependencies(manifest, &document, include_dev)? {
            if let Some((name, version)) = self.owned.get(&dependency.target) {
                self.reached.insert(name.clone(), version.clone());
                // Record exact declaring items; excluded consumer package versions
                // and unrelated registry dependencies remain untouched.
                self.declarations.push(PathDependency {
                    declaration: dependency.declaration.clone(),
                    item_path: dependency.item_path.clone(),
                    target: dependency.target.clone(),
                });
            }
            self.visit(&dependency.target, false, visited)?;
        }
        Ok(())
    }

    fn path_dependencies(
        &mut self,
        manifest: &Path,
        document: &DocumentMut,
        include_dev: bool,
    ) -> Result<Vec<PathDependency>> {
        let mut dependencies = Vec::new();
        for table_path in dependency_table_paths(document) {
            if table_path.first().is_some_and(|part| part == "workspace")
                || (!include_dev
                    && table_path.last().is_some_and(|part| part == "dev-dependencies"))
            {
                continue;
            }
            let table = table_path
                .iter()
                .try_fold(document.as_item(), |item, key| item.as_table_like()?.get(key))
                .and_then(Item::as_table_like);
            let Some(table) = table else {
                continue;
            };
            for (alias, item) in table.iter() {
                let mut item_path = table_path.clone();
                item_path.push(alias.to_owned());
                if let Some(dependency) = self.resolve_dependency(manifest, item_path, item)? {
                    dependencies.push(dependency);
                }
            }
        }
        Ok(dependencies)
    }

    fn resolve_dependency(
        &mut self,
        manifest: &Path,
        mut item_path: Vec<String>,
        item: &Item,
    ) -> Result<Option<PathDependency>> {
        let Some(table) = item.as_table_like() else {
            return Ok(None);
        };
        let mut declaration = manifest.to_path_buf();
        let path = if table.get("workspace").and_then(Item::as_bool) == Some(true) {
            let Some((workspace, document)) = self.workspace_manifest(manifest)? else {
                return Ok(None);
            };
            declaration = workspace;
            item_path = vec![
                "workspace".to_owned(),
                "dependencies".to_owned(),
                item_path.pop().expect("dependency alias"),
            ];
            item_at(&document, &item_path)
                .and_then(Item::as_table_like)
                .and_then(|spec| spec.get("path"))
                .and_then(Item::as_str)
                .map(ToOwned::to_owned)
        } else {
            table.get("path").and_then(Item::as_str).map(ToOwned::to_owned)
        };
        let Some(path) = path else {
            return Ok(None);
        };
        let Some(target) = self.dependency_target(&declaration, &path)? else {
            return Ok(None);
        };
        Ok(Some(PathDependency { declaration, item_path, target }))
    }

    fn dependency_target(&self, declaration: &Path, path: &str) -> Result<Option<PathBuf>> {
        let target = declaration.parent().context("manifest parent")?.join(path).join("Cargo.toml");
        if target.ancestors().any(|ancestor| {
            std::fs::symlink_metadata(ancestor)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        }) {
            return Ok(None);
        }
        let Ok(target) = target.canonicalize() else {
            return Ok(None);
        };
        Ok(is_tracked(self.repo_root, &target, self.tracked).then_some(target))
    }

    fn workspace_manifest(&mut self, manifest: &Path) -> Result<Option<(PathBuf, DocumentMut)>> {
        let member = source_document(manifest, &mut self.sources)?;
        if let Some(explicit) = member
            .get("package")
            .and_then(Item::as_table_like)
            .and_then(|package| package.get("workspace"))
            .and_then(Item::as_str)
        {
            let Some(candidate) = self.dependency_target(manifest, explicit)? else {
                return Ok(None);
            };
            let document = source_document(&candidate, &mut self.sources)?;
            if document.get("workspace").is_some() {
                return Ok(Some((candidate, document)));
            }
            return Ok(None);
        }
        for directory in manifest.parent().into_iter().flat_map(Path::ancestors) {
            if !directory.starts_with(self.repo_root) {
                break;
            }
            let candidate = directory.join("Cargo.toml");
            if !is_tracked(self.repo_root, &candidate, self.tracked) {
                continue;
            }
            let document = source_document(&candidate, &mut self.sources)?;
            if document.get("workspace").is_some() {
                return Ok(Some((candidate, document)));
            }
        }
        Ok(None)
    }

    fn reject_owned_patches(&self, manifest: &Path, document: &DocumentMut) -> Result<()> {
        for key in ["patch", "replace"] {
            let Some(table) = document.get(key).and_then(Item::as_table_like) else {
                continue;
            };
            for (_, item) in table.iter() {
                let Some(specs) = item.as_table_like() else {
                    continue;
                };
                for spec in std::iter::once(item).chain(specs.iter().map(|(_, spec)| spec)) {
                    let Some(path) = spec
                        .as_table_like()
                        .and_then(|entry| entry.get("path"))
                        .and_then(Item::as_str)
                    else {
                        continue;
                    };
                    let target =
                        manifest.parent().context("manifest parent")?.join(path).join("Cargo.toml");
                    if target.canonicalize().is_ok_and(|target| self.owned.contains_key(&target)) {
                        bail!(
                            "owned path package in nested [{key}] at '{}' requires an explicit release update",
                            manifest.display()
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn rewrite_item(&mut self, dependency: &PathDependency, new_version: &str) -> Result<()> {
        let original = super::source_text(&dependency.declaration, &mut self.sources)?;
        self.updates.entry(dependency.declaration.clone()).or_insert(original);
        // Apply only to a verified owned path declaration, retaining all other
        // nested package versions and dependency specifications.
        let mut document = self.updates[&dependency.declaration].parse::<DocumentMut>()?;
        if let Some(item) = cargo::get_item_mut(document.as_item_mut(), &dependency.item_path) {
            set_dependency_version(item, new_version);
        }
        self.updates.insert(dependency.declaration.clone(), document.to_string());
        Ok(())
    }

    fn rewrite_versions(&mut self, new_version: &str) -> Result<()> {
        for dependency in std::mem::take(&mut self.declarations) {
            self.rewrite_item(&dependency, new_version)?;
        }
        Ok(())
    }
}

fn item_at<'a>(document: &'a DocumentMut, path: &[String]) -> Option<&'a Item> {
    path.iter().try_fold(document.as_item(), |item, key| item.as_table_like()?.get(key))
}

#[cfg(test)]
#[path = "versioning_nested_tests.rs"]
mod tests;
