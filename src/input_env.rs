//! Normalization of the process environment and command line before parsing.
//!
//! The container action exposes every action input to the entrypoint as an
//! `INPUT_<NAME>` environment variable whose name preserves hyphens, so the
//! `tag-prefix` input arrives as `INPUT_TAG-PREFIX`. Inputs the caller left
//! unset arrive as empty strings rather than as missing variables, and a Docker
//! action's `args:` list is static, so empty values can also reach the CLI as
//! explicit flag values such as `--token ""`.
//!
//! Both shapes of "not provided" are stripped here, before clap parses
//! anything, which leaves this resolution order:
//!
//! 1. an explicit, non-empty CLI flag;
//! 2. otherwise a non-empty `INPUT_<NAME>` variable;
//! 3. otherwise the legacy `GITHUB_TOKEN`, `OWNER`, and `REPO_NAME` variables,
//!    which seed their `INPUT_*` counterparts;
//! 4. otherwise the subcommand's own clap default, which is how subcommands
//!    with conflicting defaults for the same input keep them.

use std::{collections::BTreeMap, ffi::OsString};

/// Prefix GitHub Actions gives every input variable exposed to a container.
const INPUT_PREFIX: &str = "INPUT_";

/// Historic environment variables that seed an input when it is not provided.
///
/// These predate the container action's `INPUT_*` variables and stay supported
/// for direct CLI use; the `INPUT_*` variable wins when both are set.
const LEGACY_FALLBACKS: &[(&str, &str)] =
    &[("INPUT_TOKEN", "GITHUB_TOKEN"), ("INPUT_OWNER", "OWNER"), ("INPUT_REPO-NAME", "REPO_NAME")];

/// A single edit to apply to the process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EnvAction {
    /// Drop the variable so clap treats the input as not provided.
    Remove,
    /// Copy the named variable's value into the input variable.
    CopyFrom(&'static str),
}

/// Normalizes the environment, then strips empty flag values from `args`.
///
/// # Safety
///
/// This mutates the process environment, which is not thread safe. Call it as
/// the first statement of `main`, before any other thread exists and before any
/// other code reads the environment.
pub unsafe fn normalize_inputs<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    let environment = collect_environment();
    for (key, action) in environment_plan(&environment) {
        match action {
            EnvAction::Remove => {
                // SAFETY: upheld by this function's own contract.
                unsafe { std::env::remove_var(&key) };
            }
            EnvAction::CopyFrom(source) => {
                if let Some(value) = std::env::var_os(source) {
                    // SAFETY: upheld by this function's own contract.
                    unsafe { std::env::set_var(&key, value) };
                }
            }
        }
    }

    sanitize_args(args)
}

/// Reads the environment, lossily decoding values so a non-UTF-8 variable
/// anywhere in the environment cannot abort startup.
fn collect_environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| {
            Some((key.into_string().ok()?, value.to_string_lossy().into_owned()))
        })
        .collect()
}

/// Computes the environment edits that make "empty means not provided" true.
fn environment_plan(environment: &BTreeMap<String, String>) -> BTreeMap<String, EnvAction> {
    let mut actions = BTreeMap::new();

    for (key, value) in environment {
        if key.starts_with(INPUT_PREFIX) && value.trim().is_empty() {
            actions.insert(key.clone(), EnvAction::Remove);
        }
    }

    for (input_key, legacy_key) in LEGACY_FALLBACKS {
        if is_provided(environment.get(*input_key)) {
            continue;
        }

        if is_provided(environment.get(*legacy_key)) {
            actions.insert((*input_key).to_owned(), EnvAction::CopyFrom(legacy_key));
        }
    }

    actions
}

/// Reports whether a variable carries a value that counts as provided.
fn is_provided(value: Option<&String>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

/// Drops `--flag <empty>` pairs and `--flag=` assignments from the arguments.
///
/// Everything after a bare `--` is passed through untouched.
fn sanitize_args<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    let mut sanitized = Vec::new();
    let mut remaining = args.into_iter().peekable();

    while let Some(arg) = remaining.next() {
        let text = arg.to_string_lossy().into_owned();

        if text == "--" {
            sanitized.push(arg);
            sanitized.extend(remaining);
            break;
        }

        let Some(flag) = text.strip_prefix("--").filter(|flag| !flag.is_empty()) else {
            sanitized.push(arg);
            continue;
        };

        if let Some((_, value)) = flag.split_once('=') {
            if !value.trim().is_empty() {
                sanitized.push(arg);
            }
            continue;
        }

        if remaining.peek().is_some_and(is_blank) {
            remaining.next();
            continue;
        }

        sanitized.push(arg);
    }

    sanitized
}

/// Reports whether an argument is empty or only whitespace.
fn is_blank(value: &OsString) -> bool {
    value.to_string_lossy().trim().is_empty()
}

#[cfg(test)]
// The environment lock is deliberately held for the whole test body; that is
// the point of the helper, so the tightening suggestion does not apply.
#[allow(clippy::significant_drop_tightening)]
pub mod testing {
    //! Shared helpers for the environment-sensitive CLI tests.

    use std::{
        collections::BTreeMap,
        ffi::OsString,
        sync::{Mutex, MutexGuard, PoisonError},
    };

    use super::{INPUT_PREFIX, LEGACY_FALLBACKS};

    /// Serializes every test that touches the process environment, which is
    /// global to the test binary and shared by all test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Restores the input-related environment when the test body finishes.
    struct EnvGuard {
        snapshot: BTreeMap<String, OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for key in managed_keys() {
                // SAFETY: the guard holds `ENV_LOCK`, so no other test thread
                // is reading or writing the environment concurrently.
                unsafe { std::env::remove_var(&key) };
            }

            for (key, value) in &self.snapshot {
                // SAFETY: as above.
                unsafe { std::env::set_var(key, value) };
            }
        }
    }

    /// Names this helper takes ownership of for the duration of a test.
    fn managed_keys() -> Vec<String> {
        let mut keys: Vec<String> = std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .filter(|key| key.starts_with(INPUT_PREFIX))
            .collect();
        keys.extend(LEGACY_FALLBACKS.iter().map(|(_, legacy)| (*legacy).to_owned()));
        keys.sort();
        keys.dedup();
        keys
    }

    /// Runs `body` with `overrides` applied to the environment, restoring every
    /// `INPUT_*` and legacy variable afterwards even if `body` panics.
    pub fn with_env<T>(overrides: &[(&str, &str)], body: impl FnOnce() -> T) -> T {
        let lock = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let snapshot = managed_keys()
            .into_iter()
            .filter_map(|key| std::env::var_os(&key).map(|value| (key, value)))
            .collect();
        let guard = EnvGuard { snapshot, _lock: lock };

        for key in managed_keys() {
            // SAFETY: the guard holds `ENV_LOCK`, so no other test thread is
            // reading or writing the environment concurrently.
            unsafe { std::env::remove_var(&key) };
        }
        for (key, value) in overrides {
            // SAFETY: as above.
            unsafe { std::env::set_var(key, value) };
        }

        let result = body();
        drop(guard);
        result
    }
}

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
mod tests {
    use std::ffi::OsString;

    use super::{EnvAction, environment_plan, sanitize_args};

    fn environment(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
    }

    fn args(values: &[&str]) -> Vec<String> {
        sanitize_args(values.iter().map(OsString::from))
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn empty_input_vars_are_removed() {
        let plan = environment_plan(&environment(&[("INPUT_BASE-BRANCH", "")]));
        assert_eq!(plan.get("INPUT_BASE-BRANCH"), Some(&EnvAction::Remove));
    }

    #[test]
    fn whitespace_only_input_vars_are_removed() {
        let plan = environment_plan(&environment(&[("INPUT_LABELS", "  \t ")]));
        assert_eq!(plan.get("INPUT_LABELS"), Some(&EnvAction::Remove));
    }

    #[test]
    fn non_empty_input_vars_are_left_alone() {
        let plan = environment_plan(&environment(&[("INPUT_TAG-PREFIX", "release-")]));
        assert!(plan.is_empty());
    }

    #[test]
    fn non_input_vars_are_never_removed() {
        let plan = environment_plan(&environment(&[("PATH", ""), ("GITHUB_TOKEN", "")]));
        assert!(plan.is_empty());
    }

    #[test]
    fn legacy_env_seeds_a_missing_input_var() {
        let plan = environment_plan(&environment(&[
            ("GITHUB_TOKEN", "ghp_legacy"),
            ("OWNER", "threatflux"),
            ("REPO_NAME", "github_actions"),
        ]));

        assert_eq!(plan.get("INPUT_TOKEN"), Some(&EnvAction::CopyFrom("GITHUB_TOKEN")));
        assert_eq!(plan.get("INPUT_OWNER"), Some(&EnvAction::CopyFrom("OWNER")));
        assert_eq!(plan.get("INPUT_REPO-NAME"), Some(&EnvAction::CopyFrom("REPO_NAME")));
    }

    #[test]
    fn input_var_wins_over_legacy_env() {
        let plan = environment_plan(&environment(&[
            ("INPUT_TOKEN", "ghp_input"),
            ("GITHUB_TOKEN", "ghp_legacy"),
        ]));
        assert!(plan.is_empty());
    }

    #[test]
    fn empty_input_var_falls_back_to_legacy_env() {
        let plan = environment_plan(&environment(&[
            ("INPUT_REPO-NAME", "   "),
            ("REPO_NAME", "github_actions"),
        ]));
        assert_eq!(plan.get("INPUT_REPO-NAME"), Some(&EnvAction::CopyFrom("REPO_NAME")));
    }

    #[test]
    fn blank_legacy_env_never_seeds_an_input_var() {
        let plan = environment_plan(&environment(&[("INPUT_OWNER", ""), ("OWNER", " ")]));
        assert_eq!(plan.get("INPUT_OWNER"), Some(&EnvAction::Remove));
    }

    #[test]
    fn empty_flag_values_are_dropped_from_args() {
        assert_eq!(
            args(&["bin", "update", "--token", "", "--owner", "threatflux"]),
            ["bin", "update", "--owner", "threatflux"]
        );
    }

    #[test]
    fn whitespace_only_flag_values_are_dropped_from_args() {
        assert_eq!(args(&["bin", "release", "--base-branch", "  "]), ["bin", "release"]);
    }

    #[test]
    fn inline_empty_flag_values_are_dropped_from_args() {
        assert_eq!(
            args(&["bin", "status", "--repo-name=", "--owner=threatflux"]),
            ["bin", "status", "--owner=threatflux"]
        );
    }

    #[test]
    fn non_empty_flag_values_are_preserved_in_args() {
        let line = ["bin", "pin", "--repo", ".", "--dry-run", "true"];
        assert_eq!(args(&line), line);
    }

    #[test]
    fn args_after_a_double_dash_are_preserved() {
        let line = ["bin", "update", "--", "--title", ""];
        assert_eq!(args(&line), line);
    }
}
