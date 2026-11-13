"""Exercise real planning and signed expected-head mutation boundaries with API fixtures."""

import copy
import unittest
from unittest import mock

from fixtures import (
    CENTRAL_SOURCE,
    COMMIT,
    SOURCE,
    FakeGitHub,
    app_identity,
    policy,
    upgraded,
)
from api import PolicyError
from control import (
    app_access,
    branch_name,
    capability_requirements,
    plan,
    repository_plan,
    write_proposal,
)
import control


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

    def test_selector_failure_names_only_the_public_repository_and_managed_file(self):
        self.github.files[".github/workflows/ci.yml"] = b"name: Changed layout\n"
        with self.assertRaises(PolicyError) as failure:
            repository_plan(self.github, self.item, self.config)
        self.assertEqual(
            str(failure.exception),
            "ThreatFlux/example:.github/workflows/ci.yml: "
            "Reviewed workflow job context changed",
        )
        self.assertEqual(self.github.writes, [])

    def test_missing_image_or_compiler_evidence_prevents_every_repository_plan(self):
        actual = copy.deepcopy(self.config["target"])
        actual["images"].pop("trixie")
        with self.assertRaises(PolicyError):
            plan(self.github, self.config, "ThreatFlux", "", actual)
        self.assertEqual(self.github.writes, [])

    def test_private_profile_is_unneeded_but_exact_installed_scope_is_required(self):
        self.github.installed_ids = [123, 456]
        with self.assertRaises(PolicyError):
            app_access(self.github, self.config, self.row, app_identity())
        self.github.installed_ids = [123, 123]
        with self.assertRaises(PolicyError):
            app_access(self.github, self.config, self.row, app_identity())
        self.github.installed_ids = [123]
        noop = dict(self.row, needs_workflows_write=False)
        self.assertEqual(
            app_access(self.github, self.config, noop, app_identity(""))[
                "scoped_repository_id"
            ],
            123,
        )
        self.assertFalse(any(path.startswith("/apps/") for path in self.github.reads))

    def test_readonly_capability_is_unverified_even_when_source_plan_is_complete(self):
        report = plan(self.github, self.config, "ThreatFlux", "", self.config["target"])
        capability_requirements(self.config, report)
        self.assertTrue(report["complete"])
        self.assertEqual(report["installed_capability"]["status"], "UNVERIFIED")
        self.assertEqual(
            report["installed_capability"]["workflows_write_required_by"],
            [self.item["repository"]],
        )
        self.assertFalse(any(path.startswith("/apps/") for path in self.github.reads))
        self.assertEqual(self.github.writes, [])

    def test_numeric_or_client_identifier_maps_to_the_same_reviewed_app(self):
        for value in ["5205173", "Iv23liKqGk6HjSZl41QE"]:
            identity = dict(app_identity(), identifier=value)
            proof = app_access(self.github, self.config, self.row, identity)
            self.assertEqual(proof["id"], 5205173)
            self.assertEqual(proof["installation_id"], 37)

    def test_wrong_app_outputs_or_missing_required_permission_block_before_iat_reads(
        self,
    ):
        variants = [
            {"identifier": "another-app"},
            {"identifier": "5205174"},
            {"slug": "another-app"},
            {"installation_id": ""},
            {"installation_id": "0"},
            {"installation_id": "-1"},
            {"installation_id": "37\n"},
            {"workflows": ""},
            {"workflows": "read"},
        ]
        with mock.patch.object(self.github, "pages") as request:
            for changed in variants:
                with self.subTest(changed=changed), self.assertRaises(PolicyError):
                    app_access(
                        self.github,
                        self.config,
                        self.row,
                        dict(app_identity(), **changed),
                    )
        request.assert_not_called()

    def test_unnecessary_workflow_grant_is_not_accepted_for_noop_or_policy_writer(self):
        row = dict(self.row, needs_workflows_write=False)
        with self.assertRaises(PolicyError):
            app_access(self.github, self.config, row, app_identity("write"))

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

    def test_target_head_advance_after_existing_proof_blocks_reuse(self):
        write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        original = control.existing_proposal
        count = len(self.github.writes)

        def advance(*arguments):
            result = original(*arguments)
            self.github.source = "e" * 40
            return result

        with mock.patch.object(
            control, "existing_proposal", side_effect=advance
        ), self.assertRaises(PolicyError):
            write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(count, len(self.github.writes))

    def test_foreign_fork_namespace_pr_does_not_block_or_get_changed(self):
        foreign = {
            "user": {"login": "contributor"},
            "head": {
                "ref": branch_name(self.config),
                "repo": {"id": 456},
                "sha": "f" * 40,
            },
        }
        self.github.pulls = [copy.deepcopy(foreign)]
        result = write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(result["status"], "proposed")
        self.assertEqual(self.github.pulls[0], foreign)
        self.assertEqual(len(self.github.pulls), 2)
        self.assertEqual(self.github.pulls[-1]["head"]["repo"]["id"], 123)

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

    def test_central_policy_revocation_after_ref_creation_prevents_commit(self):
        self.github.on_ref_created = lambda client: setattr(
            client, "central_source", "e" * 40
        )
        with self.assertRaises(PolicyError):
            write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(self.github.branch, SOURCE)
        self.assertFalse(
            any(
                path == "graphql" or path.endswith("/pulls")
                for path, _ in self.github.writes
            )
        )

    def test_central_policy_revocation_during_commit_prevents_pr(self):
        self.github.on_graphql = lambda client: setattr(
            client, "central_source", "e" * 40
        )
        with self.assertRaises(PolicyError):
            write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(self.github.branch, COMMIT)
        self.assertFalse(any(path.endswith("/pulls") for path, _ in self.github.writes))

    def test_central_policy_revocation_blocks_existing_proposal_reuse_claim(self):
        write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        original = control.existing_proposal
        count = len(self.github.writes)

        def revoke(*arguments):
            result = original(*arguments)
            self.github.central_source = "e" * 40
            return result

        with mock.patch.object(
            control, "existing_proposal", side_effect=revoke
        ), self.assertRaises(PolicyError):
            write_proposal(self.github, self.config, self.row, CENTRAL_SOURCE)
        self.assertEqual(count, len(self.github.writes))

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
