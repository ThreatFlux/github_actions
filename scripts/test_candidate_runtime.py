"""Source/actor rejection and publication-isolation regressions for CI."""

import unittest
from pathlib import Path
from unittest.mock import patch

import candidate_runtime as candidate

SOURCE = "a" * 40
CONTEXT = {
    "CANDIDATE_RUNTIME": "true",
    "CANDIDATE_SOURCE_SHA": SOURCE,
    "RELEASE_PREP": "false",
    "GITHUB_EVENT_NAME": "workflow_dispatch",
    "GITHUB_REPOSITORY": candidate.UPSTREAM,
    "REPOSITORY_IS_FORK": "false",
    "GITHUB_SHA": SOURCE,
    "GITHUB_REF": "refs/heads/codex/review-publisher",
    "GITHUB_REF_NAME": "codex/review-publisher",
    "GITHUB_ACTOR": "reviewed-maintainer",
    "GITHUB_TRIGGERING_ACTOR": "rerun-maintainer",
}


def workflow_steps():
    workflow = (
        Path(__file__)
        .resolve()
        .parents[1]
        .joinpath(".github/workflows/docker.yml")
        .read_text(encoding="utf-8")
    )
    sections = {}
    for section in workflow.split("      - name: ")[1:]:
        sections[section.splitlines()[0]] = section
    return workflow, sections


class CandidateRuntimeTests(unittest.TestCase):
    def test_ordinary_publication_makes_no_candidate_api_requests(self):
        with patch.object(candidate, "api") as api:
            self.assertEqual(candidate.admit({}), {"enabled": "false"})
        api.assert_not_called()

    def test_explicit_dispatch_emits_only_the_full_source_candidate_tag(self):
        def api(endpoint):
            if "/git/ref/" in endpoint:
                return {"object": {"sha": SOURCE}}
            return {"permission": "write"}

        with patch.object(candidate, "api", side_effect=api) as calls:
            with patch.object(
                candidate.subprocess, "check_output", return_value=SOURCE + "\n"
            ):
                outputs = candidate.admit(CONTEXT)
        self.assertEqual(
            outputs["image"], f"ghcr.io/threatflux/github_actions:candidate-{SOURCE}"
        )
        self.assertEqual(outputs["source"], SOURCE)
        self.assertEqual(outputs["enabled"], "true")
        self.assertEqual(len(calls.call_args_list), 3)
        endpoints = {call.args[0] for call in calls.call_args_list}
        self.assertIn(
            f"repos/{candidate.UPSTREAM}/collaborators/reviewed-maintainer/permission",
            endpoints,
        )
        self.assertIn(
            f"repos/{candidate.UPSTREAM}/collaborators/rerun-maintainer/permission",
            endpoints,
        )

    def test_rejects_unsafe_dispatch_context_before_api_or_publication(self):
        rejected = {
            "CANDIDATE_RUNTIME": ["yes", "false"],
            "CANDIDATE_SOURCE_SHA": ["", "a" * 7, "b" * 40, "A" * 40],
            "RELEASE_PREP": ["true", ""],
            "GITHUB_EVENT_NAME": ["push", "pull_request", "workflow_run"],
            "GITHUB_REPOSITORY": ["fork/github_actions", "ThreatFlux/another"],
            "REPOSITORY_IS_FORK": ["true", ""],
            "GITHUB_SHA": ["b" * 40],
            "GITHUB_REF": ["refs/heads/main", "refs/tags/v0.7.12", "refs/heads/codex/"],
            "GITHUB_REF_NAME": ["main"],
        }
        for key, values in rejected.items():
            for value in values:
                context = dict(CONTEXT, **{key: value})
                with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                    candidate.candidate_source(context)

    def test_rejects_missing_actor_or_unprivileged_rerun(self):
        for key in ["GITHUB_ACTOR", "GITHUB_TRIGGERING_ACTOR"]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                candidate.trusted_actors(dict(CONTEXT, **{key: ""}))
        for permission in ["read", "triage", "none", None]:
            with self.subTest(permission=permission), self.assertRaises(ValueError):
                candidate.validate_evidence(
                    SOURCE,
                    SOURCE,
                    {"object": {"sha": SOURCE}},
                    [{"permission": "admin"}, {"permission": permission}],
                )

    def test_rejects_changed_checkout_or_advanced_remote_branch(self):
        for checkout, remote in [
            ("b" * 40, SOURCE),
            (SOURCE, "b" * 40),
            (SOURCE, None),
        ]:
            with self.subTest(checkout=checkout, remote=remote), self.assertRaises(
                ValueError
            ):
                candidate.validate_evidence(
                    SOURCE,
                    checkout,
                    {"object": {"sha": remote}},
                    [{"permission": "maintain"}],
                )

    def test_workflow_keeps_candidate_tags_separate_from_normal_aliases(self):
        _, sections = workflow_steps()
        for name in [
            "Extract runtime metadata",
            "Extract base image metadata",
            "Build base image",
            "Check Docker Hub credentials",
        ]:
            self.assertIn("steps.candidate.outputs.enabled != 'true'", sections[name])
        self.assertIn(
            "steps.candidate.outputs.image || steps.meta.outputs.tags",
            sections["Build and push runtime image (non-PR)"],
        )
        labels = sections["Label the source-bound candidate runtime"]
        self.assertIn("steps.candidate.outputs.enabled == 'true'", labels)
        self.assertIn("flavor: latest=false", labels)
        self.assertIn("org.opencontainers.image.revision=${{ github.sha }}", labels)
        self.assertIn(
            "org.opencontainers.image.source=https://github.com/${{ github.repository }}",
            labels,
        )
        self.assertIn(
            "steps.candidate_meta.outputs.labels || steps.meta.outputs.labels",
            sections["Build and push runtime image (non-PR)"],
        )

    def test_candidate_image_retains_both_architectures_and_all_scans(self):
        workflow, sections = workflow_steps()
        self.assertIn("- arch: amd64", workflow)
        self.assertIn("- arch: arm64", workflow)
        self.assertIn(
            "platforms: ${{ steps.platforms.outputs.value }}",
            sections["Build and push runtime image (non-PR)"],
        )
        self.assertIn("needs.build.outputs.image_ref", sections["Pull image"])
        self.assertIn("needs.build.outputs.image_ref", sections["Run Trivy scanner"])
        self.assertIn("needs.build.outputs.image_ref", sections["Generate SBOM"])
        self.assertIn(
            "scripts/check_static_image.py",
            sections["Validate static runtime and HTTPS API behavior"],
        )
        self.assertIn(
            "scripts/smoke_container_https.py",
            sections["Validate static runtime and HTTPS API behavior"],
        )

    def test_candidate_signing_waits_for_all_scans_and_never_hands_off(self):
        workflow, sections = workflow_steps()
        self.assertIn("needs: [build, scan, sbom]", workflow)
        self.assertIn(
            "needs.build.outputs.candidate != 'true'",
            workflow.split("  release-handoff:")[1],
        )
        self.assertIn(
            'cosign sign --yes -a "source-sha=${GITHUB_SHA}" -a runtime-channel=candidate "$IMAGE"',
            sections["Sign container image"],
        )


if __name__ == "__main__":
    unittest.main()
