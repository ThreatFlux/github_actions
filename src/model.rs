use std::path::PathBuf;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkflowAction {
    pub file: PathBuf,
    pub line_number: usize,
    pub indentation: String,
    pub list_prefix: String,
    pub action_slug: String,
    pub owner: String,
    pub repository: String,
    pub version: String,
    pub inline_comment: Option<String>,
    pub original_line: String,
}

impl WorkflowAction {
    #[must_use]
    pub fn repository_slug(&self) -> String {
        format!("{}/{}", self.owner, self.repository)
    }

    #[must_use]
    pub fn is_pinned(&self) -> bool {
        is_full_length_sha(&self.version)
    }

    #[must_use]
    pub fn logical_version(&self) -> String {
        if self.is_pinned() {
            self.version_hint().unwrap_or_else(|| self.version.clone())
        } else {
            self.version.clone()
        }
    }

    #[must_use]
    pub fn rendered_line(&self, commit_sha: &str, version_label: &str) -> String {
        let mut comment = String::from(version_label);

        if let Some(existing_comment) = self.extra_comment()
            && !existing_comment.is_empty()
            && existing_comment != version_label
        {
            comment.push_str(" | ");
            comment.push_str(&existing_comment);
        }

        format!(
            "{}{}uses: {}@{}  # {}",
            self.indentation, self.list_prefix, self.action_slug, commit_sha, comment
        )
    }

    fn version_hint(&self) -> Option<String> {
        if !self.is_pinned() {
            return None;
        }

        let comment = self.inline_comment.as_deref()?.trim();
        let (candidate, _) = comment.split_once('|').unwrap_or((comment, ""));
        let candidate = candidate.trim();

        if looks_like_version_hint(candidate) { Some(candidate.to_owned()) } else { None }
    }

    fn extra_comment(&self) -> Option<String> {
        let comment = self.inline_comment.as_deref()?.trim();
        if comment.is_empty() {
            return None;
        }

        if self.is_pinned() {
            let (candidate, remainder) = comment.split_once('|').unwrap_or((comment, ""));
            if looks_like_version_hint(candidate.trim()) {
                let remainder = remainder.trim();
                if remainder.is_empty() { None } else { Some(remainder.to_owned()) }
            } else {
                Some(comment.to_owned())
            }
        } else {
            Some(comment.to_owned())
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PinChange {
    pub file: PathBuf,
    pub line_number: usize,
    pub action_slug: String,
    pub from_version: String,
    pub to_sha: String,
    pub original_line: String,
    pub rewritten_line: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PinReport {
    pub workflow_files: usize,
    pub references_scanned: usize,
    pub already_pinned: usize,
    pub changes: Vec<PinChange>,
}

impl PinReport {
    #[must_use]
    pub fn changed_files(&self) -> usize {
        let mut files = self.changes.iter().map(|change| change.file.as_path()).collect::<Vec<_>>();
        files.sort();
        files.dedup();
        files.len()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum UpdateChangeKind {
    GitHubAction,
    CargoDependency,
}

impl UpdateChangeKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::GitHubAction => "GitHub Action",
            Self::CargoDependency => "cargo package",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UpdateChange {
    pub kind: UpdateChangeKind,
    pub file: PathBuf,
    pub line_number: Option<usize>,
    pub subject: String,
    pub from_version: String,
    pub to_version: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FileUpdate {
    pub file: PathBuf,
    pub updated_content: String,
}

#[must_use]
pub fn is_full_length_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn looks_like_version_hint(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && !trimmed.contains(char::is_whitespace)
        && (trimmed.starts_with('v')
            || trimmed.chars().next().is_some_and(|character| character.is_ascii_digit())
            || matches!(trimmed, "main" | "master" | "stable" | "beta" | "nightly"))
}
