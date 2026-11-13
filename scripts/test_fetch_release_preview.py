"""Exercise preview data fetching with API fixtures and mocked HTTPS only."""

import base64
import copy
import hashlib
import http.client
import io
import json
import os
import secrets
import ssl
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

import fetch_release_preview as preview

REPOSITORY = "ThreatFlux/example"
COMMIT = "a" * 40
TREE = "b" * 40
PREFIX = f"/repos/{REPOSITORY}/git"


def blob_fixture(data):
    framed = b"blob " + str(len(data)).encode("ascii") + b"\0" + data
    sha = hashlib.sha1(framed, usedforsecurity=False).hexdigest()
    encoded = base64.b64encode(data).decode("ascii")
    return sha, {
        "sha": sha,
        "size": len(data),
        "encoding": "base64",
        "content": "\n".join(
            encoded[index : index + 60] for index in range(0, len(encoded), 60)
        ),
    }


class SourceFixture:
    def __init__(self):
        self.entries = []
        self.files = {}
        self.calls = []
        self.responses = {
            f"{PREFIX}/commits/{COMMIT}": {"sha": COMMIT, "tree": {"sha": TREE}},
            f"{PREFIX}/trees/{TREE}?recursive=1": {
                "sha": TREE,
                "truncated": False,
                "tree": self.entries,
            },
        }
        self.add_file(
            "Cargo.toml", b'[package]\r\nname = "example"\r\nversion = "1.2.3"\r\n'
        )
        self.add_file("Cargo.lock", b"# lock data\nversion = 4\n")
        self.add_directory("fuzz")
        self.add_file("fuzz/Cargo.toml", b'[dependencies]\nexample = { path = ".." }\n')
        self.add_file("fuzz/Cargo.lock", b"# independent lock\nversion = 4\n")
        self.add_directory("src")
        self.add_file("src/lib.rs", b"candidate code must never be downloaded")
        self.add_directory(".cargo")
        self.add_file(
            ".cargo/config.toml",
            b"candidate Cargo configuration must never be downloaded",
        )

    def add_directory(self, path):
        self.entries.append(
            {"path": path, "mode": "040000", "type": "tree", "sha": "c" * 40}
        )

    def add_file(self, path, data):
        sha, blob = blob_fixture(data)
        self.entries.append(
            {
                "path": path,
                "type": "blob",
                "mode": "100644",
                "sha": sha,
                "size": len(data),
                "url": "https://untrusted.invalid/never-follow-this",
            }
        )
        self.files[path] = data
        self.responses[f"{PREFIX}/blobs/{sha}"] = blob

    def get(self, route):
        self.calls.append(route)
        return copy.deepcopy(self.responses[route])

    def entry(self, path):
        return next(entry for entry in self.entries if entry["path"] == path)


class PreviewDataTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.workspace = Path(directory.name)
        self.source = SourceFixture()

    def fetch(self, repository=REPOSITORY, sha=COMMIT, output=".preview-target"):
        return preview.fetch_preview(
            self.source, repository, sha, output, self.workspace
        )

    def test_fetches_all_nested_cargo_bytes_and_no_executable_or_configuration(self):
        self.assertEqual(self.fetch(), 4)
        output = self.workspace / ".preview-target"
        expected = {"Cargo.toml", "Cargo.lock", "fuzz/Cargo.toml", "fuzz/Cargo.lock"}
        found = {
            file.relative_to(output).as_posix()
            for file in output.rglob("*")
            if file.is_file()
        }
        self.assertEqual(found, expected)
        for path in expected:
            self.assertEqual((output / path).read_bytes(), self.source.files[path])
        for ignored in ["src/lib.rs", ".cargo/config.toml"]:
            sha = self.source.entry(ignored)["sha"]
            self.assertNotIn(f"{PREFIX}/blobs/{sha}", self.source.calls)
        self.assertEqual(len(self.source.calls), 6)

    def test_uses_git_blob_framing_known_vector(self):
        self.assertEqual(
            blob_fixture(b"hello\n")[0], "ce013625030ba8dba906f756967f9e9ca394464a"
        )

    def test_regular_executable_mode_is_copied_only_as_nonexecuting_data(self):
        self.source.entry("Cargo.toml")["mode"] = "100755"
        self.fetch()
        output = self.workspace / ".preview-target/Cargo.toml"
        self.assertEqual(output.read_bytes(), self.source.files["Cargo.toml"])
        self.assertEqual(output.stat().st_mode & 0o111, 0)

    def test_rejects_invalid_repository_and_sha_before_api_calls(self):
        for repository, sha in [
            ("../example", COMMIT),
            ("a/b/c", COMMIT),
            ("a/..", COMMIT),
            (REPOSITORY, "abc"),
        ]:
            with self.subTest(repository=repository, sha=sha):
                self.source.calls.clear()
                with self.assertRaises(preview.PreviewError):
                    self.fetch(repository, sha)
                self.assertEqual(self.source.calls, [])

    def test_rejects_commit_or_tree_identity_mismatch(self):
        routes = [f"{PREFIX}/commits/{COMMIT}", f"{PREFIX}/trees/{TREE}?recursive=1"]
        for route in routes:
            with self.subTest(route=route):
                self.source = SourceFixture()
                self.source.responses[route]["sha"] = "d" * 40
                with self.assertRaises(preview.PreviewError):
                    self.fetch()
                self.assertFalse((self.workspace / ".preview-target").exists())

    def test_rejects_truncated_or_missing_tree_completeness(self):
        for completeness in [True, None]:
            with self.subTest(completeness=completeness):
                self.source = SourceFixture()
                self.source.responses[f"{PREFIX}/trees/{TREE}?recursive=1"][
                    "truncated"
                ] = completeness
                with self.assertRaises(preview.PreviewError):
                    self.fetch()
                self.assertFalse((self.workspace / ".preview-target").exists())

    def test_rejects_unsafe_paths_without_writing_outside_workspace(self):
        paths = [
            "/Cargo.toml",
            "../Cargo.toml",
            "x/../Cargo.lock",
            "x\\Cargo.toml",
            "x//Cargo.toml",
            "./Cargo.toml",
            "C:/Cargo.toml",
            ".git/Cargo.toml",
        ]
        for path in paths:
            with self.subTest(path=path):
                self.source = SourceFixture()
                self.source.add_file(path, b"unsafe path data")
                with self.assertRaises(preview.PreviewError):
                    self.fetch()
                self.assertEqual(list(self.workspace.iterdir()), [])

    def test_rejects_selected_symlinks_submodules_and_nonregular_modes(self):
        for kind, mode in [
            ("blob", "120000"),
            ("commit", "160000"),
            ("tree", "040000"),
            ("blob", "100664"),
        ]:
            with self.subTest(kind=kind, mode=mode):
                self.source = SourceFixture()
                self.source.entry("Cargo.toml").update(type=kind, mode=mode)
                with self.assertRaises(preview.PreviewError):
                    self.fetch()
                self.assertFalse((self.workspace / ".preview-target").exists())

    def test_rejects_symlink_ancestor_duplicate_path_and_missing_root_manifest(self):
        mutations = ["ancestor", "duplicate", "missing-root"]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                self.source = SourceFixture()
                if mutation == "ancestor":
                    self.source.entry("fuzz").update(type="blob", mode="120000")
                elif mutation == "duplicate":
                    self.source.entries.append(
                        copy.deepcopy(self.source.entry("Cargo.toml"))
                    )
                else:
                    self.source.entries.remove(self.source.entry("Cargo.toml"))
                with self.assertRaises(preview.PreviewError):
                    self.fetch()
                self.assertFalse((self.workspace / ".preview-target").exists())

    def test_rejects_blob_identity_content_size_and_encoding_mismatches(self):
        for mutation in ["sha", "content", "size", "tree-size", "encoding", "base64"]:
            with self.subTest(mutation=mutation):
                self.source = SourceFixture()
                entry = self.source.entry("Cargo.toml")
                blob = self.source.responses[f"{PREFIX}/blobs/{entry['sha']}"]
                if mutation == "content":
                    data = self.source.files["Cargo.toml"]
                    blob["content"] = base64.b64encode(
                        bytes([data[0] ^ 1]) + data[1:]
                    ).decode()
                elif mutation == "sha":
                    blob["sha"] = "d" * 40
                elif mutation == "tree-size":
                    entry["size"] += 1
                elif mutation == "size":
                    blob["size"] += 1
                elif mutation == "encoding":
                    blob["encoding"] = "utf-8"
                else:
                    blob["content"] = "!!!!"
                directory = self.workspace / mutation
                with self.assertRaises(preview.PreviewError):
                    self.fetch(output=mutation)
                self.assertFalse((directory / "Cargo.toml").exists())

    def test_never_reuses_an_existing_output_directory(self):
        output = self.workspace / ".preview-target"
        output.mkdir()
        sentinel = output / "sentinel"
        sentinel.write_bytes(b"keep this file")
        with self.assertRaises(FileExistsError):
            self.fetch()
        self.assertEqual(sentinel.read_bytes(), b"keep this file")

    def test_rejects_output_escape_and_nonfresh_nested_names(self):
        for name in ["../escape", "/outside", "C:\\outside", ".git", "nested/preview"]:
            with self.subTest(name=name):
                with self.assertRaises(preview.PreviewError):
                    self.fetch(output=name)
                self.assertEqual(list(self.workspace.iterdir()), [])

    @unittest.skipIf(
        os.name == "nt", "Windows symlink creation needs developer privileges"
    )
    def test_rejects_an_output_symlink_escape(self):
        with tempfile.TemporaryDirectory() as outside:
            (self.workspace / ".preview-target").symlink_to(
                outside, target_is_directory=True
            )
            with self.assertRaises(preview.PreviewError):
                self.fetch()
            self.assertEqual(list(Path(outside).iterdir()), [])

    def test_cli_uses_only_readonly_environment_and_requested_coordinates(self):
        token = secrets.token_hex(24)
        environment = {
            "GH_TOKEN": token,
            "GITHUB_API_URL": "https://git.example/api/v3",
        }
        with (
            mock.patch.dict(os.environ, environment, clear=True),
            mock.patch.object(
                preview, "GitHubApi", return_value=self.source
            ) as factory,
            mock.patch.object(preview.Path, "cwd", return_value=self.workspace),
            redirect_stdout(io.StringIO()),
        ):
            result = preview.main(
                [
                    "--repository",
                    REPOSITORY,
                    "--sha",
                    COMMIT,
                    "--output",
                    ".preview-target",
                ]
            )
        self.assertEqual(result, 0)
        factory.assert_called_once_with(environment["GITHUB_API_URL"], token)
        self.assertEqual(
            (self.workspace / ".preview-target/Cargo.toml").read_bytes(),
            self.source.files["Cargo.toml"],
        )


class HttpsTransportTests(unittest.TestCase):
    def setUp(self):
        self.token = secrets.token_hex(24)
        self.connection = mock.Mock()
        self.response = self.connection.getresponse.return_value
        self.response.status = 200
        self.response.read.return_value = json.dumps({"value": "fixture"}).encode()

    def test_https_api_prefix_authorization_and_certificate_validation(self):
        with mock.patch.object(
            http.client, "HTTPSConnection", return_value=self.connection
        ) as factory:
            client = preview.GitHubApi("https://git.example/api/v3", self.token)
            self.assertEqual(
                client.get("/repos/ThreatFlux/example"), {"value": "fixture"}
            )
        self.assertEqual(factory.call_args.args, ("git.example", None))
        context = factory.call_args.kwargs["context"]
        self.assertTrue(context.check_hostname)
        self.assertEqual(context.verify_mode, ssl.CERT_REQUIRED)
        call = self.connection.request.call_args
        self.assertEqual(call.args, ("GET", "/api/v3/repos/ThreatFlux/example"))
        self.assertEqual(
            call.kwargs["headers"]["Authorization"], "Bearer " + self.token
        )
        self.connection.close.assert_called_once()

    def test_rejects_unsafe_api_bases_and_missing_token(self):
        for base in [
            "http://git.example",
            "file:///tmp/api",
            "https://",
            f"https://user:{self.token}@git.example",
            "https://git.example?x=1",
            "https://git.example/#fragment",
        ]:
            with self.subTest(base=base), self.assertRaises(preview.PreviewError):
                preview.GitHubApi(base, self.token)
        with self.assertRaises(preview.PreviewError):
            preview.GitHubApi("https://git.example", "")

    def test_redirects_and_http_errors_do_not_follow_or_leak_credentials(self):
        for status in [301, 302, 403, 404, 500]:
            with self.subTest(status=status):
                self.connection.reset_mock()
                self.response.status = status
                with mock.patch.object(
                    http.client, "HTTPSConnection", return_value=self.connection
                ) as factory:
                    client = preview.GitHubApi("https://git.example", self.token)
                    with self.assertRaises(preview.PreviewError) as failure:
                        client.get("/repos/ThreatFlux/example")
                self.assertNotIn(self.token, str(failure.exception))
                factory.assert_called_once()
                self.connection.request.assert_called_once()
                self.connection.close.assert_called_once()

    def test_transport_errors_never_report_the_authorization_value(self):
        self.connection.request.side_effect = http.client.HTTPException(self.token)
        with mock.patch.object(
            http.client, "HTTPSConnection", return_value=self.connection
        ):
            client = preview.GitHubApi("https://git.example", self.token)
            with self.assertRaises(preview.PreviewError) as failure:
                client.get("/repos/ThreatFlux/example")
        self.assertNotIn(self.token, str(failure.exception))
        self.connection.close.assert_called_once()


if __name__ == "__main__":
    unittest.main()
