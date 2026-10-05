"""Exercise real Git lineage, reference ambiguity, and inherited hook isolation."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from check_release_ref import git, resolve_release_source


class ReleaseSourceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        git(self.root, "init", "--initial-branch=main")
        git(self.root, "config", "user.name", "Release fixture")
        git(self.root, "config", "user.email", "release-fixture@example.invalid")
        git(self.root, "config", "commit.gpgsign", "false")
        git(self.root, "config", "tag.gpgsign", "false")
        git(self.root, "commit", "--allow-empty", "-m", "base")
        self.base = git(self.root, "rev-parse", "HEAD")
        git(self.root, "tag", "v0.7.5")
        git(self.root, "commit", "--allow-empty", "-m", "main head")
        self.head = git(self.root, "rev-parse", "HEAD")
        git(self.root, "update-ref", "refs/remotes/origin/main", self.head)

    def test_merged_branch_tag_and_full_sha_are_pinned(self) -> None:
        for source, expected in (
            ("main", self.head), ("refs/heads/main", self.head),
            ("v0.7.5", self.base), ("refs/tags/v0.7.5", self.base),
            (self.base, self.base),
        ):
            with self.subTest(source=source):
                self.assertEqual(resolve_release_source(self.root, source), expected)

    def test_unmerged_branch_and_tag_fail_lineage_check(self) -> None:
        git(self.root, "checkout", "-b", "unmerged")
        git(self.root, "commit", "--allow-empty", "-m", "untrusted change")
        git(self.root, "update-ref", "refs/remotes/origin/unmerged", "HEAD")
        git(self.root, "tag", "v99.0.0")
        for source in ("unmerged", "v99.0.0", git(self.root, "rev-parse", "HEAD")):
            with self.subTest(source=source), self.assertRaises(subprocess.CalledProcessError):
                resolve_release_source(self.root, source)

    def test_revision_expressions_options_and_pull_refs_are_rejected(self) -> None:
        for source in ("HEAD~1", "main^{commit}", "--help", "refs/pull/1/merge", "main\nother", ""):
            with self.subTest(source=source), self.assertRaises((ValueError, subprocess.CalledProcessError)):
                resolve_release_source(self.root, source)

    def test_ambiguous_branch_and_tag_fail(self) -> None:
        git(self.root, "branch", "v0.7.5", self.head)
        git(self.root, "update-ref", "refs/remotes/origin/v0.7.5", self.head)
        with self.assertRaises(ValueError):
            resolve_release_source(self.root, "v0.7.5")

    def test_fixture_git_cannot_modify_inherited_repository(self) -> None:
        with tempfile.TemporaryDirectory() as inherited:
            sentinel = Path(inherited) / "config"
            sentinel.write_text("sentinel: not a git repository\n")
            before = sentinel.read_bytes()
            with patch.dict(os.environ, {
                "GIT_DIR": inherited, "GIT_WORK_TREE": inherited,
                "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "core.bare",
                "GIT_CONFIG_VALUE_0": "true", "GIT_CONFIG_PARAMETERS": "'core.bare=true'",
            }):
                git(self.root, "config", "fixture.isolated", "true")
                self.assertEqual(resolve_release_source(self.root, "main"), self.head)
            self.assertEqual(sentinel.read_bytes(), before)
            self.assertEqual(git(self.root, "config", "fixture.isolated"), "true")


if __name__ == "__main__":
    unittest.main()
