"""Replay phase detection and post-publication image promotion without writes."""

import base64
import os
import secrets
import shlex
import tempfile
import unittest
from pathlib import Path

from test_reusable_release_gate import run_gate_script

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = (ROOT / ".github/workflows/auto-release.yml").read_text(encoding="utf-8")
PROMOTE = (ROOT / "scripts/promote_release_image.sh").read_text(encoding="utf-8")
DIGEST = "sha256:" + "a" * 64
IMAGE = "ghcr.io/threatflux/github_actions"
PIN = f"FROM {IMAGE}:0.7.10@{DIGEST}\n"
GH_SH = r"""#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GH_LOG"
case "$*" in
  *commits/main*) printf '%040d\n' 1;;
  *contents/Cargo.toml*) printf '%s\n' "$MANIFEST_BASE64";;
  *git/ref/tags/*)
    if [[ "$FAKE_TAG_HTTP" != transport ]]; then printf 'HTTP/2.0 %s Status\n' "$FAKE_TAG_HTTP"; fi
    [[ "$FAKE_TAG_HTTP" == 200 ]];;
  *contents/runtime/Dockerfile*ref=v0.7.10*) printf '%s\n' "$PIN_BASE64";;
  *contents/release/Dockerfile*ref=v0.7.10*) printf '%s\n' "$RELEASE_PIN_BASE64";;
  *git/matching-refs/tags/v*) printf '%s\n' "$FAKE_VERSIONS";;
  *) exit 71;;
esac
"""
DOCKER_SH = r"""#!/usr/bin/env bash
case "$1 $2 $3" in
  'buildx imagetools inspect') printf '%s\n' "$FAKE_DIGEST";;
  'buildx imagetools create') printf '%s\n' "$*" >> "$WRITE_LOG";;
  'run --rm '*) printf 'github-actions-maintainer %s\n' "$FAKE_CLI_VERSION";;
  'login ghcr.io '*) cat >/dev/null; echo login >> "$WRITE_LOG";;
  *) exit 72;;
esac
"""


def step(name):
    return WORKFLOW.split("      - name: " + name + "\n", 1)[1].split(
        "\n      - name:", 1
    )[0]


def shell_step(name):
    body = step(name).split("        run: |\n", 1)[1]
    return "\n".join(line[10:] for line in body.splitlines())


def encoded(text):
    return base64.b64encode(text.encode()).decode()


def replay(script, **overrides):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        for name, content in [("gh", GH_SH), ("docker", DOCKER_SH)]:
            path = root / name
            path.write_text(content, encoding="utf-8")
            path.chmod(0o755)
        output, writes, calls = (root / name for name in ["output", "writes", "calls"])
        env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"])
        env.update(
            GITHUB_REPOSITORY="ThreatFlux/github_actions",
            GITHUB_OUTPUT=str(output),
            GH_LOG=str(calls),
            WRITE_LOG=str(writes),
            REQUESTED_PHASE="auto",
            FAKE_TAG_HTTP="404",
            MANIFEST_BASE64=encoded('[package]\nversion = "0.7.10"\n'),
            PIN_BASE64=encoded(PIN),
            RELEASE_PIN_BASE64=encoded(PIN),
            FAKE_DIGEST=DIGEST,
            FAKE_CLI_VERSION="0.7.10",
            FAKE_VERSIONS="0.7.10\n0.7.9",
            VERSION="0.7.10",
            GITHUB_TOKEN=secrets.token_hex(24),
            GITHUB_ACTOR="test-actor",
            EXPECTED_IMAGE=IMAGE,
            EXPECTED_DIGEST=DIGEST,
            RELEASED="true",
        )
        env.update(overrides)
        result = run_gate_script(script, env)
        return result, captured(output), captured(writes), captured(calls)


def captured(path):
    return path.read_text(encoding="utf-8") if path.exists() else ""


def promoted_aliases(writes):
    command = next(line for line in writes.splitlines() if line.startswith("buildx "))
    arguments = shlex.split(command)
    return [arguments[n + 1] for n, value in enumerate(arguments) if value == "--tag"]


class ReleaseAliasTests(unittest.TestCase):
    def test_only_actual_404_is_an_absent_tag(self):
        for status in ["403", "500", "transport"]:
            with self.subTest(status=status):
                result, output, writes, _ = replay(
                    shell_step("Determine target SHA and phase"), FAKE_TAG_HTTP=status
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("refusing to infer absence", result.stderr)
                self.assertNotIn("phase=", output)
                self.assertEqual(writes, "")
        result, output, _, _ = replay(shell_step("Determine target SHA and phase"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("phase=tag", output)
        self.assertIn("skip=false", output)

    def test_forced_tag_of_an_existing_version_is_a_no_op(self):
        result, output, writes, _ = replay(
            shell_step("Determine target SHA and phase"),
            FAKE_TAG_HTTP="200",
            REQUESTED_PHASE="tag",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("skip=true", output)
        self.assertEqual(writes, "")
        self.assertIn(
            "if: steps.target.outputs.skip != 'true'", step("Check CI status")
        )

    def test_existing_tag_auto_mode_still_selects_the_bump_phase(self):
        result, output, _, _ = replay(
            shell_step("Determine target SHA and phase"), FAKE_TAG_HTTP="200"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("phase=bump", output)
        self.assertIn("skip=false", output)

    def test_promotion_follows_publisher_and_requires_a_confirmed_release(self):
        publish = WORKFLOW.index(
            "      - name: Tag and publish the committed version\n"
        )
        promote = WORKFLOW.index(
            "      - name: Tag the verified image with the release version\n"
        )
        dispatch = WORKFLOW.index("      - name: Trigger tag pipelines\n")
        self.assertLess(publish, promote)
        self.assertLess(promote, dispatch)
        self.assertNotIn("imagetools create", WORKFLOW[:publish])
        wiring = step("Tag the verified image with the release version")
        self.assertIn("if: steps.release.outputs.released == 'true'", wiring)
        self.assertIn("run: bash scripts/promote_release_image.sh", wiring)
        for refusal in ["existing-tag", "behind-version", "head-mismatch"]:
            with self.subTest(refusal=refusal):
                result, _, writes, calls = replay(
                    'if [[ "$RELEASED" == true ]]; then\n' + PROMOTE + "\nfi",
                    RELEASED="false",
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((writes, calls), ("", ""))

    def test_confirmed_release_promotes_only_its_sealed_digest(self):
        result, _, writes, calls = replay(PROMOTE)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            promoted_aliases(writes), [f"{IMAGE}:0.7.10", f"{IMAGE}:0.7", f"{IMAGE}:0"]
        )
        self.assertIn(IMAGE + "@" + DIGEST, writes)
        self.assertIn("runtime/Dockerfile?ref=v0.7.10", calls)
        self.assertIn("release/Dockerfile?ref=v0.7.10", calls)
        self.assertIn("Cargo.toml?ref=v0.7.10", calls)
        self.assertNotIn("ref=main", calls)

    def test_old_release_recovery_preserves_newer_line_aliases(self):
        for versions, suffixes in [
            ("0.7.11\n0.7.10", ["0.7.10"]),
            ("0.8.0\n0.7.10", ["0.7.10", "0.7"]),
        ]:
            with self.subTest(versions=versions):
                result, _, writes, _ = replay(PROMOTE, FAKE_VERSIONS=versions)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    promoted_aliases(writes), [IMAGE + ":" + v for v in suffixes]
                )

    def test_pin_or_runtime_mismatch_never_writes_an_alias(self):
        for overrides in [
            {"RELEASE_PIN_BASE64": encoded(PIN.replace("a" * 64, "b" * 64))},
            {"EXPECTED_DIGEST": "sha256:" + "b" * 64},
            {"FAKE_CLI_VERSION": "0.7.9"},
            {"MANIFEST_BASE64": encoded('[package]\nversion = "0.7.9"\n')},
            {"FAKE_DIGEST": "sha256:" + "b" * 64},
            {"FAKE_VERSIONS": "0.7.9"},
        ]:
            with self.subTest(overrides=overrides):
                result, _, writes, _ = replay(PROMOTE, **overrides)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(writes, "")
