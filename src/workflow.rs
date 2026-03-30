use std::{
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Context, Result, bail};
use regex::Regex;
use walkdir::WalkDir;

use crate::model::{PinChange, WorkflowAction};

static USES_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<indent>\s*)(?P<list>-\s*)?uses:\s*(?P<uses>[^#\s]+)\s*(?:#\s*(?P<comment>.*))?$",
    )
    .expect("valid workflow uses regex")
});

#[derive(Debug, Clone, Eq, PartialEq)]
struct ParsedActionTarget {
    action_slug: String,
    owner: String,
    repository: String,
    version: String,
}

pub fn discover_workflow_files(repo_root: &Path, workflows_path: &Path) -> Result<Vec<PathBuf>> {
    let workflow_root = if workflows_path.is_absolute() {
        workflows_path.to_path_buf()
    } else {
        repo_root.join(workflows_path)
    };

    if !workflow_root.exists() {
        bail!("workflow directory '{}' does not exist", workflow_root.display());
    }

    let mut files = WalkDir::new(&workflow_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            matches!(entry.path().extension().and_then(|ext| ext.to_str()), Some("yml" | "yaml"))
        })
        .map(walkdir::DirEntry::into_path)
        .collect::<Vec<_>>();

    files.sort();
    Ok(files)
}

pub fn scan_workflow(path: &Path) -> Result<Vec<WorkflowAction>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read workflow '{}'", path.display()))?;

    let mut actions = Vec::new();

    for (index, line) in content.lines().enumerate() {
        let Some(captures) = USES_LINE_RE.captures(line) else {
            continue;
        };

        let uses_value = captures.name("uses").expect("uses capture is required").as_str();

        let Some(parsed) = parse_action_target(uses_value) else {
            continue;
        };

        let indentation =
            captures.name("indent").map_or(String::new(), |capture| capture.as_str().to_owned());
        let list_prefix =
            captures.name("list").map_or(String::new(), |capture| capture.as_str().to_owned());
        let inline_comment = captures.name("comment").map(|capture| capture.as_str().to_owned());

        actions.push(WorkflowAction {
            file: path.to_path_buf(),
            line_number: index + 1,
            indentation,
            list_prefix,
            action_slug: parsed.action_slug,
            owner: parsed.owner,
            repository: parsed.repository,
            version: parsed.version,
            inline_comment,
            original_line: line.to_owned(),
        });
    }

    Ok(actions)
}

pub fn apply_changes(changes: &[PinChange]) -> Result<()> {
    let mut by_file = changes.iter().fold(
        std::collections::BTreeMap::<&Path, Vec<&PinChange>>::new(),
        |mut grouped, change| {
            grouped.entry(change.file.as_path()).or_default().push(change);
            grouped
        },
    );

    for (path, file_changes) in &mut by_file {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read workflow '{}'", path.display()))?;
        let rewritten = apply_changes_to_content(&content, file_changes)?;

        fs::write(path, rewritten)
            .with_context(|| format!("failed to write workflow '{}'", path.display()))?;
    }

    Ok(())
}

pub fn apply_changes_to_content(content: &str, changes: &[&PinChange]) -> Result<String> {
    let mut lines = content.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut sorted_changes = changes.to_vec();

    sorted_changes.sort_by(|left, right| right.line_number.cmp(&left.line_number));

    for change in sorted_changes {
        let line_index = change.line_number - 1;
        if line_index >= lines.len() {
            bail!("cannot rewrite content: line {} is outside the file", change.line_number);
        }

        lines[line_index].clone_from(&change.rewritten_line);
    }

    let rewritten =
        if content.ends_with('\n') { format!("{}\n", lines.join("\n")) } else { lines.join("\n") };

    Ok(rewritten)
}

fn parse_action_target(raw: &str) -> Option<ParsedActionTarget> {
    if raw.contains("${{")
        || raw.starts_with("./")
        || raw.starts_with("../")
        || raw.starts_with('/')
        || raw.starts_with("docker://")
    {
        return None;
    }

    let (action_slug, version) = raw.rsplit_once('@')?;
    let parts = action_slug.split('/').collect::<Vec<_>>();

    if parts.len() < 2 {
        return None;
    }

    Some(ParsedActionTarget {
        action_slug: action_slug.to_owned(),
        owner: parts[0].to_owned(),
        repository: parts[1].to_owned(),
        version: version.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::{apply_changes, discover_workflow_files, scan_workflow};
    use crate::model::PinChange;

    #[test]
    fn scan_workflow_collects_github_actions() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow = temp_dir.path().join("ci.yml");
        fs::write(
            &workflow,
            r"jobs:
  lint:
    steps:
      - uses: actions/checkout@v4
      - uses: github/codeql-action/init@v3 # security
      - uses: ./local-action
      - uses: docker://ghcr.io/acme/tool:latest
      - uses: ${{ matrix.action }}
",
        )
        .expect("write workflow");

        let actions = scan_workflow(&workflow).expect("scan workflow");

        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].action_slug, "actions/checkout");
        assert_eq!(actions[0].version, "v4");
        assert_eq!(actions[1].action_slug, "github/codeql-action/init");
        assert_eq!(actions[1].inline_comment.as_deref(), Some("security"));
    }

    #[test]
    fn discover_workflow_files_only_returns_yaml() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow_dir = temp_dir.path().join(".github").join("workflows");
        fs::create_dir_all(&workflow_dir).expect("create workflow directory");
        fs::write(workflow_dir.join("ci.yml"), "name: CI\n").expect("write yml workflow");
        fs::write(workflow_dir.join("release.yaml"), "name: Release\n")
            .expect("write yaml workflow");
        fs::write(workflow_dir.join("notes.txt"), "skip\n").expect("write non-workflow");

        let files = discover_workflow_files(temp_dir.path(), Path::new(".github/workflows"))
            .expect("discover workflows");

        assert_eq!(files.len(), 2);
    }

    #[test]
    fn apply_changes_rewrites_target_lines() {
        let temp_dir = tempdir().expect("tempdir");
        let workflow = temp_dir.path().join("ci.yml");
        fs::write(&workflow, "steps:\n  - uses: actions/checkout@v4\n  - uses: actions/cache@v4\n")
            .expect("write workflow");

        apply_changes(&[
            PinChange {
                file: workflow.clone(),
                line_number: 2,
                action_slug: "actions/checkout".into(),
                from_version: "v4".into(),
                to_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                original_line: "  - uses: actions/checkout@v4".into(),
                rewritten_line:
                    "  - uses: actions/checkout@0123456789abcdef0123456789abcdef01234567  # v4"
                        .into(),
            },
            PinChange {
                file: workflow.clone(),
                line_number: 3,
                action_slug: "actions/cache".into(),
                from_version: "v4".into(),
                to_sha: "89abcdef0123456789abcdef0123456789abcdef".into(),
                original_line: "  - uses: actions/cache@v4".into(),
                rewritten_line:
                    "  - uses: actions/cache@89abcdef0123456789abcdef0123456789abcdef  # v4".into(),
            },
        ])
        .expect("apply changes");

        let updated = fs::read_to_string(&workflow).expect("read rewritten workflow");
        assert!(
            updated.contains(
                "  - uses: actions/checkout@0123456789abcdef0123456789abcdef01234567  # v4"
            )
        );
        assert!(
            updated
                .contains("  - uses: actions/cache@89abcdef0123456789abcdef0123456789abcdef  # v4")
        );
    }
}
