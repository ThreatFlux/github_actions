"""Run rotate-automation-app-key.sh against fake gh and curl binaries.

The fakes record every invocation, so the tests can prove the dry run changes
nothing, the real run shares the secret with exactly the checked-in repository
list, feeds the key on stdin and records its fingerprint, drift between the list
and the organization settings stops the script, and no key material or JWT ever
reaches argv or the script's output.
"""

import base64
import hashlib
import os
import shutil

# B404: The script under test and openssl run as fixed argv lists, never via a shell.
import subprocess  # nosec B404
import re
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().with_name("rotate-automation-app-key.sh")
APP_ID = "5205173"
REPOS = "github_actions,ollama_rust_sdk"
REPO_LIST = "# test list\nollama_rust_sdk\ngithub_actions  # health repository\n\n"

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
if args[:2] == ["variable", "set"]:
    sys.exit(int(os.environ.get("FAKE_VARIABLE_SET_EXIT", "0")))
if args[:2] == ["workflow", "run"]:
    (state / "dispatched").write_text("yes")
    sys.exit(0)
if args[:2] == ["run", "list"]:
    print("101" if (state / "dispatched").exists() and "--event" in args else "100")
    sys.exit(0)
if args[:2] == ["run", "watch"]:
    sys.exit(int(os.environ.get("FAKE_WATCH_EXIT", "0")))
def visibility(variable):
    value = os.environ.get(variable, "selected")
    if value == "missing":
        sys.exit("gh: Not Found (HTTP 404)")
    print(value)
if args[0] == "api":
    if joined.endswith("/actions/secrets --jq .secrets[].name"):
        repo = joined.split("/repos/ThreatFlux/", 1)[1].split("/", 1)[0]
        print("TF_AUTOMATION_APP_PRIVATE_KEY" if repo == os.environ.get("FAKE_SHADOW_REPO") else "CODECOV_TOKEN")
    elif joined.endswith("/variables/TF_AUTOMATION_APP_ID/repositories --jq .repositories[].name"):
        print("\n".join(os.environ.get("FAKE_VAR_REPOS", os.environ["FAKE_REPOS"]).split(",")))
    elif joined.endswith("/secrets/TF_AUTOMATION_APP_PRIVATE_KEY/repositories --jq .repositories[].name"):
        print("\n".join(os.environ["FAKE_REPOS"].split(",")))
    elif joined.endswith("/variables/TF_AUTOMATION_APP_ID --jq .visibility"):
        visibility("FAKE_VAR_VISIBILITY")
    elif joined.endswith("/variables/TF_AUTOMATION_APP_ID --jq .value"):
        print(os.environ["FAKE_APP_ID"])
    elif joined.endswith("/secrets/TF_AUTOMATION_APP_PRIVATE_KEY --jq .visibility"):
        visibility("FAKE_SECRET_VISIBILITY")
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
        self.repos_file = self.root / "automation-app-repos.txt"
        self.repos_file.write_text(REPO_LIST)
        self.pem = self.root / "new-key.pem"
        self.openssl("genrsa", "-out", str(self.pem), "2048")

    def openssl(self, *args: str) -> None:
        # B603/B607: fixed openssl subcommands on temporary fixture paths.
        subprocess.run(["openssl", *args], check=True, capture_output=True)  # nosec B603 B607

    def rotate(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        # B603/B607: the script under test with fixed arguments and a fixture key.
        return subprocess.run(  # nosec B603 B607
            ["bash", str(SCRIPT), "--repos-file", str(self.repos_file), *args], env={**self.env, **env},
            stdin=subprocess.DEVNULL, capture_output=True, text=True, check=False, timeout=120,
        )

    def fingerprint(self) -> str:
        # B603/B607: fixed openssl subcommands on the temporary fixture key.
        public = subprocess.run(  # nosec B603 B607
            ["openssl", "rsa", "-in", str(self.pem), "-pubout", "-outform", "DER"],
            check=True, capture_output=True).stdout
        return "SHA256:" + base64.b64encode(hashlib.sha256(public).digest()).decode()

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
        self.assertIn(f"fingerprint {self.fingerprint()}", result.stdout)
        self.assertIn(f"will be shared with: {REPOS}", result.stdout)
        self.assertIn(f"would record TF_AUTOMATION_APP_KEY_ROTATED_AT=<rotation time, UTC> and"
                      f" TF_AUTOMATION_APP_KEY_FINGERPRINT={self.fingerprint()}", result.stdout)
        self.assertIn("Dry run: nothing changed", result.stdout)
        self.assertNotIn("secret set", self.gh_log())
        self.assertNotIn("variable set", self.gh_log())
        self.assertNotIn("workflow run", self.gh_log())
        self.assertTrue(self.pem.exists())
        self.assert_no_key_material(result, key_lines)

    def test_rotation_shares_the_listed_repositories_records_the_key_and_removes_the_file(self) -> None:
        key_bytes = self.pem.read_bytes()
        key_lines = self.key_lines()
        fingerprint = self.fingerprint()
        result = self.rotate("--yes", str(self.pem))
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.gh_log()
        self.assertIn(
            f"secret set TF_AUTOMATION_APP_PRIVATE_KEY --org ThreatFlux --visibility selected --repos {REPOS}\n", log)
        self.assertEqual((self.state / "secret-stdin").read_bytes(), key_bytes)
        self.assertNotIn("variable set TF_AUTOMATION_APP_ID", log)
        scope = "--org ThreatFlux --visibility selected --repos github_actions"
        rotated = re.search(rf"^variable set TF_AUTOMATION_APP_KEY_ROTATED_AT {scope} --body (\S+)$", log, re.M)
        self.assertIsNotNone(rotated, log)
        self.assertRegex(rotated.group(1), r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
        self.assertIn(f"variable set TF_AUTOMATION_APP_KEY_FINGERPRINT {scope} --body {fingerprint}\n", log)
        # The date is written before the fingerprint, and both before the health run.
        self.assertLess(log.index("ROTATED_AT"), log.index("KEY_FINGERPRINT"))
        self.assertLess(log.index("KEY_FINGERPRINT"), log.index("workflow run"))
        self.assertIn("workflow run automation-app-health.yml --repo ThreatFlux/github_actions --ref main", log)
        self.assertIn("run watch 101 --repo ThreatFlux/github_actions --exit-status", log)
        self.assertIn("https://github.com/organizations/ThreatFlux/settings/apps/threatflux-automation", result.stdout)
        self.assertFalse(self.pem.exists())
        self.assert_no_key_material(result, key_lines)

    def test_failed_record_warns_with_the_commands_and_carries_on(self) -> None:
        fingerprint = self.fingerprint()
        result = self.rotate("--yes", str(self.pem), FAKE_VARIABLE_SET_EXIT="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("could not record the rotation", result.stderr)
        self.assertIn(f"gh variable set TF_AUTOMATION_APP_KEY_FINGERPRINT --org ThreatFlux --visibility selected"
                      f" --repos github_actions --body '{fingerprint}'", result.stderr)
        self.assertFalse(self.pem.exists())

    def test_drift_stops_before_any_change(self) -> None:
        for env, message in (
            ({"FAKE_REPOS": "github_actions"}, "TF_AUTOMATION_APP_PRIVATE_KEY is not shared with listed repositories:"
                                               " ollama_rust_sdk"),
            ({"FAKE_REPOS": f"{REPOS},lifeflux"}, "TF_AUTOMATION_APP_PRIVATE_KEY is shared with repositories that are"
                                                  " not listed: lifeflux"),
            ({"FAKE_VAR_REPOS": "ollama_rust_sdk"}, "TF_AUTOMATION_APP_ID is not shared with listed repositories:"
                                                    " github_actions"),
            ({"FAKE_SECRET_VISIBILITY": "all"}, "TF_AUTOMATION_APP_PRIVATE_KEY is visible to all repositories"),
            ({"FAKE_VAR_VISIBILITY": "private"}, "TF_AUTOMATION_APP_ID is visible to private repositories"),
        ):
            with self.subTest(env=env):
                result = self.rotate("--yes", str(self.pem), **env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertIn("rerun with --sync-repos", result.stderr)
                self.assertNotIn("secret set", self.gh_log())
                self.assertNotIn("variable set", self.gh_log())
                self.assertTrue(self.pem.exists())

    def test_sync_repos_shares_secret_and_variable_with_exactly_the_list(self) -> None:
        result = self.rotate("--yes", "--sync-repos", str(self.pem),
                             FAKE_REPOS=f"{REPOS},lifeflux", FAKE_VAR_REPOS="github_actions")
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.gh_log()
        self.assertIn(f"variable set TF_AUTOMATION_APP_ID --org ThreatFlux --visibility selected --repos {REPOS}"
                      f" --body {APP_ID}\n", log)
        self.assertIn(
            f"secret set TF_AUTOMATION_APP_PRIVATE_KEY --org ThreatFlux --visibility selected --repos {REPOS}\n", log)
        self.assertLess(log.index("variable set TF_AUTOMATION_APP_ID"), log.index("secret set"))

    def test_missing_secret_is_created_for_the_listed_repositories(self) -> None:
        result = self.rotate("--yes", str(self.pem), FAKE_SECRET_VISIBILITY="missing")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("does not exist as an organization secret", result.stderr)
        self.assertIn(
            f"secret set TF_AUTOMATION_APP_PRIVATE_KEY --org ThreatFlux --visibility selected --repos {REPOS}\n",
            self.gh_log())

    def test_check_repos_needs_no_key_and_reports_drift(self) -> None:
        result = self.rotate("--check-repos")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("match", result.stdout)
        result = self.rotate("--check-repos", FAKE_VAR_REPOS=f"{REPOS},lifeflux")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("TF_AUTOMATION_APP_ID is shared with repositories that are not listed: lifeflux", result.stderr)
        self.assertNotIn("set", self.gh_log().replace("--visibility", ""))
        self.assertNotEqual(self.rotate("--check-repos", str(self.pem)).returncode, 0)

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

    def test_repository_secret_overriding_the_org_secret_is_refused(self) -> None:
        """A same-name repository secret would keep the old key in use, so the script stops before changing anything."""
        result = self.rotate("--yes", str(self.pem), FAKE_SHADOW_REPO="ollama_rust_sdk")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("overrides the organization secret in: ollama_rust_sdk", result.stderr)
        self.assertNotIn("secret set", self.gh_log())
        self.assertTrue(self.pem.exists())

    def test_without_confirmation_a_non_interactive_run_refuses(self) -> None:
        result = self.rotate(str(self.pem))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("rerun with --yes", result.stderr)
        self.assertNotIn("secret set", self.gh_log())
        self.assertTrue(self.pem.exists())


if __name__ == "__main__":
    unittest.main()
