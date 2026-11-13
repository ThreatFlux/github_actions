"""Exercise real planning and signed expected-head mutation boundaries with API fixtures."""

import copy
import unittest

from fixtures import CENTRAL_SOURCE, COMMIT, SOURCE, FakeGitHub, policy, upgraded
from api import PolicyError
from control import (
    app_access,
    branch_name,
    capability_profile,
    plan,
    repository_plan,
    write_proposal,
)


class ControlTests(unittest.TestCase):
    def setUp(self):
        self.config = upgraded()
        self.github = FakeGitHub()
        self.item = self.config["repositories"][0]
        self.row = repository_plan(self.github, self.item, self.config)

    def test_current_target_is_true_noop_without_branch_or_pr_mutations(self):
        config = policy()
        result = plan(self.github, config, "ThreatFlux", "", config["target"])
        self.assertTrue(result["complete"])
        self.assertEqual(result["changed_repositories"], 0)
        self.assertEqual(self.github.writes, [])
        self.assertEqual(
            self.github.files["Cargo.toml"], b'[package]\nrust-version="1.100.0"\n'
        )

    def test_new_target_changes_only_reviewed_stable_selector_and_exact_path(self):
        self.assertEqual(
            [change["path"] for change in self.row["changes"]],
            [".github/workflows/ci.yml"],
        )
        self.assertTrue(self.row["needs_workflows_write"])
        self.assertEqual(self.row["source_sha"], SOURCE)
        self.assertNotEqual(
            self.row["changes"][0]["before_sha256"],
            self.row["changes"][0]["after_sha256"],
        )
        self.assertEqual(self.github.writes, [])

    def test_missing_image_or_compiler_evidence_prevents_every_repository_plan(self):
        actual = copy.deepcopy(self.config["target"])
        actual["images"].pop("trixie")
        with self.assertRaises(PolicyError):
            plan(self.github, self.config, "ThreatFlux", "", actual)
        self.assertEqual(self.github.writes, [])

    def test_public_profile_is_not_treated_as_an_installed_token_grant(self):
        self.github.installed_ids = [123, 456]
        with self.assertRaises(PolicyError):
            app_access(self.github, self.config, self.row, "threatflux-automation")
        self.github.installed_ids = [123]
        self.github.permissions.pop("workflows")
        with self.assertRaises(PolicyError):
            app_access(self.github, self.config, self.row, "threatflux-automation")
        noop = dict(self.row, needs_workflows_write=False)
        self.assertEqual(
            app_access(self.github, self.config, noop, "threatflux-automation")[
                "scoped_repository_id"
            ],
            123,
        )

    def test_changed_workflow_permission_block_is_reported_before_mutations(self):
        self.github.permissions.pop("workflows")
        report = plan(self.github, self.config, "ThreatFlux", "", self.config["target"])
        capability_profile(self.github, self.config, report)
        self.assertFalse(report["complete"])
        self.assertEqual(
            report["blocked_workflow_permission_repositories"],
            [self.item["repository"]],
        )
        self.assertEqual(self.github.writes, [])

    def test_signed_commit_uses_expected_head_and_never_forces_main(self):
        result = write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(result["status"], "proposed")
        mutation = next(
            payload for path, payload in self.github.writes if path == "graphql"
        )
        self.assertEqual(mutation["input"]["expectedHeadOid"], SOURCE)
        self.assertTrue(
            mutation["input"]["branch"]["refName"].startswith("codex/org-rust-")
        )
        self.assertNotEqual(mutation["input"]["branch"]["refName"], "main")
        self.assertFalse(
            any("force" in str(payload) for _, payload in self.github.writes)
        )

    def test_second_identical_run_reuses_owned_verified_pr_without_mutations(self):
        first = write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        count = len(self.github.writes)
        second = write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(first["pull_request"], second["pull_request"])
        self.assertEqual(second["status"], "existing-proposal")
        self.assertEqual(count, len(self.github.writes))

    def test_human_owned_branch_and_pr_are_preserved(self):
        self.github.branch = SOURCE
        with self.assertRaises(PolicyError):
            write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(self.github.writes, [])
        self.github.pulls = [
            {
                "user": {"login": "maintainer"},
                "head": {
                    "ref": branch_name(self.config),
                    "repo": {"id": 123},
                    "sha": SOURCE,
                },
            }
        ]
        with self.assertRaises(PolicyError):
            write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(self.github.writes, [])

    def test_default_branch_advance_stops_before_ref_creation(self):
        self.github.on_source = lambda client: setattr(client, "source", "e" * 40)
        with self.assertRaises(PolicyError):
            write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(self.github.writes, [])

    def test_branch_race_fails_expected_head_without_overwriting_moved_branch(self):
        self.github.on_graphql = lambda client: setattr(client, "branch", "e" * 40)
        with self.assertRaises(PolicyError):
            write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(self.github.branch, "e" * 40)
        self.assertFalse(
            any(
                path == "graphql" or path.endswith("/pulls")
                for path, _ in self.github.writes
            )
        )

    def test_unsigned_or_wrong_actor_commit_never_gets_a_pr(self):
        for attribute, value in [("verified", False), ("author", "maintainer")]:
            github = FakeGitHub()
            setattr(github, attribute, value)
            with self.assertRaises(PolicyError):
                write_proposal(github, self.config, self.row, CENTRAL_SOURCE)
            self.assertFalse(any(path.endswith("/pulls") for path, _ in github.writes))

    def test_signed_commit_with_extra_path_or_changed_mode_never_gets_a_pr(self):
        for mutation in ["mode", "extra"]:
            github = FakeGitHub()
            base_tree = github.tree

            def tree(repository, source):
                identity, entries = base_tree(repository, source)
                if source == COMMIT:
                    if mutation == "mode":
                        entries[".github/workflows/ci.yml"]["mode"] = "100755"
                    else:
                        entries["unexpected"] = {
                            "mode": "100644",
                            "type": "blob",
                            "sha": "f" * 40,
                        }
                return identity, entries

            github.tree = tree
            with self.subTest(mutation=mutation), self.assertRaises(PolicyError):
                write_proposal(github, self.config, self.row, CENTRAL_SOURCE)
            self.assertFalse(any(path.endswith("/pulls") for path, _ in github.writes))

    def test_policy_proposal_namespace_does_not_collide_with_target_rollout(self):
        self.assertNotEqual(branch_name(self.config), branch_name(self.config, True))
        self.assertTrue(
            branch_name(self.config, True).startswith("codex/org-rust-policy-")
        )

    def test_private_fork_archived_or_identity_changed_target_is_rejected(self):
        original = self.github.api
        for field, value in [
            ("private", True),
            ("fork", True),
            ("archived", True),
            ("id", 456),
        ]:

            def response(path, payload=None, missing=False):
                result = original(path, payload, missing)
                return (
                    dict(result, **{field: value})
                    if path == "/repos/" + self.item["repository"]
                    else result
                )

            self.github.api = response
            with self.subTest(field=field), self.assertRaises(PolicyError):
                repository_plan(self.github, self.item, self.config)
            self.github.api = original

    def test_existing_bot_pr_content_or_base_drift_is_not_overwritten(self):
        write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.github.proposed_sha = "f" * 40
        count = len(self.github.writes)
        with self.assertRaises(PolicyError):
            write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(count, len(self.github.writes))


if __name__ == "__main__":
    unittest.main()
