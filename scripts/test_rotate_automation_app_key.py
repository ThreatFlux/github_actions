"""Run rotate-automation-app-key.sh against fake gh and curl binaries.

The fakes record every invocation, so the tests can prove the dry run changes
nothing, the real run keeps the secret's repository list and feeds the key on
stdin, and no key material or JWT ever reaches argv or the script's output.
"""

import os
import shutil

# B404: The script under test and openssl run as fixed argv lists, never via a shell.
import subprocess  # nosec B404
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().with_name("rotate-automation-app-key.sh")
APP_ID = "5205173"
REPOS = "github_actions,ollama_rust_sdk"

FAKE_GH = r'''#!/usr/bin/env python3
import os, sys
from pathlib import Path
state = Path(os.environ["FAKE_STATE"])
args = sys.argv[1:]
with open(state / "gh.log", "a") as log:
    log.write(" ".join(args) + "\n")
joined = " ".join(args)
if args[:2] == ["secret", "set"]:
    (state / "secret-stdin").write_bytes(sys.stdin.buffer.read())
    sys.exit(int(os.environ.get("FAKE_SET_EXIT", "0")))
if args[:2] == ["workflow", "run"]:
    (state / "dispatched").write_text("yes")
    sys.exit(0)
if args[:2] == ["run", "list"]:
    print("101" if (state / "dispatched").exists() and "--event" in args else "100")
    sys.exit(0)
if args[:2] == ["run", "watch"]:
    sys.exit(int(os.environ.get("FAKE_WATCH_EXIT", "0")))
if args[0] == "api":
    if "/variables/TF_AUTOMATION_APP_ID" in joined:
        print(os.environ["FAKE_APP_ID"])
    elif joined.endswith("/repositories --jq .repositories[].name"):
        print("\n".join(os.environ["FAKE_REPOS"].split(",")))
    elif "/secrets/TF_AUTOMATION_APP_PRIVATE_KEY" in joined:
        print("selected")
    elif "/user" in joined:
        print("wroersma")
    else:
        sys.exit("unexpected gh api call: " + joined)
    sys.exit(0)
sys.exit("unexpected gh call: " + joined)
'''

FAKE_CURL = r'''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
state = Path(os.environ["FAKE_STATE"])
with open(state / "curl.log", "a") as log:
    log.write(" ".join(sys.argv[1:]) + "\n")
headers = sys.stdin.read()
(state / "curl-stdin").write_text(headers)
if not headers.startswith("Authorization: Bearer ") or headers.count(".") != 2:
    print(json.dumps({"message": "A JSON web token could not be decoded"}) + "\n401")
    sys.exit(0)
print(json.dumps({"id": int(os.environ["FAKE_RETURNED_APP_ID"]), "slug": "threatflux-automation"}) + "\n200")
'''


@unittest.skipUnless(shutil.which("bash") and shutil.which("openssl") and shutil.which("jq"),
                     "needs bash, openssl, and jq")
class RotateKeyTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.state = self.root / "state"
        self.state.mkdir()
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        for name, body in (("gh", FAKE_GH), ("curl", FAKE_CURL)):
            path = bin_dir / name
            path.write_text(body)
            path.chmod(0o755)
        self.env = {
            **os.environ,
            "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            "FAKE_STATE": str(self.state),
            "FAKE_APP_ID": APP_ID,
            "FAKE_RETURNED_APP_ID": APP_ID,
            "FAKE_REPOS": REPOS,
            "ROTATE_POLL_SECONDS": "0",
            "ROTATE_POLL_ATTEMPTS": "3",
        }
        self.pem = self.root / "new-key.pem"
        self.openssl("genrsa", "-out", str(self.pem), "2048")

    def openssl(self, *args: str) -> None:
        # B603/B607: fixed openssl subcommands on temporary fixture paths.
        subprocess.run(["openssl", *args], check=True, capture_output=True)  # nosec B603 B607

    def rotate(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        # B603/B607: the script under test with fixed arguments and a fixture key.
        return subprocess.run(  # nosec B603 B607
            ["bash", str(SCRIPT), *args], env={**self.env, **env}, stdin=subprocess.DEVNULL,
            capture_output=True, text=True, check=False, timeout=120,
        )

    def gh_log(self) -> str:
        path = self.state / "gh.log"
        return path.read_text() if path.exists() else ""

    def assert_no_key_material(self, result: subprocess.CompletedProcess, key_lines: list[str]) -> None:
        output = result.stdout + result.stderr
        for line in key_lines:
            self.assertNotIn(line, output)
        self.assertNotIn("Bearer", output)
        curl_args = (self.state / "curl.log").read_text() if (self.state / "curl.log").exists() else ""
        self.assertNotIn("Bearer", curl_args)
        self.assertNotIn("Bearer", self.gh_log())

    def key_lines(self) -> list[str]:
        return [line for line in self.pem.read_text().splitlines() if line and not line.startswith("-----")]

    def test_dry_run_validates_and_changes_nothing(self) -> None:
        key_lines = self.key_lines()
        result = self.rotate("--dry-run", str(self.pem))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("fingerprint SHA256:", result.stdout)
        self.assertIn(f"stays shared with: {REPOS}", result.stdout)
        self.assertIn("Dry run: nothing changed", result.stdout)
        self.assertNotIn("secret set", self.gh_log())
        self.assertNotIn("workflow run", self.gh_log())
        self.assertTrue(self.pem.exists())
        self.assert_no_key_material(result, key_lines)

    def test_rotation_keeps_repositories_proves_the_key_and_removes_the_file(self) -> None:
        key_bytes = self.pem.read_bytes()
        key_lines = self.key_lines()
        result = self.rotate("--yes", str(self.pem))
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.gh_log()
        self.assertIn(
            f"secret set TF_AUTOMATION_APP_PRIVATE_KEY --org ThreatFlux --visibility selected --repos {REPOS}\n", log)
        self.assertEqual((self.state / "secret-stdin").read_bytes(), key_bytes)
        self.assertIn("workflow run automation-app-health.yml --repo ThreatFlux/github_actions --ref main", log)
        self.assertIn("run watch 101 --repo ThreatFlux/github_actions --exit-status", log)
        self.assertIn("https://github.com/organizations/ThreatFlux/settings/apps/threatflux-automation", result.stdout)
        self.assertFalse(self.pem.exists())
        self.assert_no_key_material(result, key_lines)

    def test_failed_health_run_keeps_the_file(self) -> None:
        result = self.rotate("--yes", str(self.pem), FAKE_WATCH_EXIT="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("the health run failed", result.stderr)
        self.assertTrue(self.pem.exists())

    def test_key_of_another_app_is_refused_before_any_change(self) -> None:
        result = self.rotate("--yes", str(self.pem), FAKE_RETURNED_APP_ID="42")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not 5205173", result.stderr)
        self.assertNotIn("secret set", self.gh_log())
        self.assertTrue(self.pem.exists())

    def test_non_rsa_and_encrypted_keys_are_refused(self) -> None:
        ec_key = self.root / "ec.pem"
        self.openssl("ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", str(ec_key))
        encrypted = self.root / "encrypted.pem"
        self.openssl("genrsa", "-aes256", "-passout", "pass:secret", "-out", str(encrypted), "2048")
        not_a_key = self.root / "notes.pem"
        not_a_key.write_text("hello\n")
        for path in (ec_key, encrypted, not_a_key):
            with self.subTest(key=path.name):
                result = self.rotate("--yes", str(path))
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("secret set", self.gh_log())
                self.assertTrue(path.exists())

    def test_without_confirmation_a_non_interactive_run_refuses(self) -> None:
        result = self.rotate(str(self.pem))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("rerun with --yes", result.stderr)
        self.assertNotIn("secret set", self.gh_log())
        self.assertTrue(self.pem.exists())


if __name__ == "__main__":
    unittest.main()
