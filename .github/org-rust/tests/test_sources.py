"""Mock HTTPS only; bind Git objects and official two-platform image metadata."""

import base64
import hashlib
import json
import secrets
import unittest
import urllib.error
import urllib.request
from email.message import Message
from unittest import mock

import fixtures
from api import DockerBlobRedirect, GitHub, NoRedirect, PolicyError, fetch, object_id
from targets import RustImages, stable_channel


class SourceTests(unittest.TestCase):
    def test_http_failure_reports_public_endpoint_and_numeric_rate_headers_only(self):
        secret = secrets.token_hex(16)
        url = (
            f"https://user:{secret}@api.github.com/apps/example?token={secret}#{secret}"
        )
        headers = Message()
        for name, value in {
            "X-RateLimit-Remaining": "0",
            "X-RateLimit-Reset": "1794550000",
            "Retry-After": "60",
            "Authorization": "Bearer " + secret,
            "Set-Cookie": secret,
        }.items():
            headers[name] = value
        error = urllib.error.HTTPError(url, 403, secret, headers, None)
        with mock.patch("api.urllib.request.build_opener") as builder:
            builder.return_value.open.side_effect = error
            with self.assertRaises(PolicyError) as failure:
                fetch(url, secret, {"private": secret})
        self.assertEqual(
            str(failure.exception),
            "HTTPS request failed at api.github.com/apps/example: status=403; "
            "x-ratelimit-remaining=0; x-ratelimit-reset=1794550000; retry-after=60",
        )
        self.assertNotIn(secret, str(failure.exception))

    def test_http_diagnostics_reject_header_text_and_preserve_404_semantics(self):
        headers = Message()
        headers["Retry-After"] = "60; unexpected text"
        headers["X-RateLimit-Remaining"] = "0\nInjected: value"
        for status in [429, 404]:
            error = urllib.error.HTTPError(
                "https://api.github.com/repos/ThreatFlux/example",
                status,
                "",
                headers,
                None,
            )
            with mock.patch("api.urllib.request.build_opener") as builder:
                builder.return_value.open.side_effect = error
                if status == 404:
                    self.assertEqual(fetch(error.url), (None, {"status": 404}))
                else:
                    with self.assertRaises(PolicyError) as failure:
                        fetch(error.url)
                    self.assertEqual(
                        str(failure.exception),
                        "HTTPS request failed at api.github.com/repos/ThreatFlux/example: status=429",
                    )

    def test_real_api_wrapper_allows_immutable_three_dot_compare_path(self):
        path = (
            "/repos/ThreatFlux/example/compare/"
            + fixtures.SOURCE
            + "..."
            + fixtures.COMMIT
        )
        github = GitHub(secrets.token_hex(16))
        with mock.patch(
            "api.fetch", return_value=(b'{"status":"ahead"}', {})
        ) as request:
            self.assertEqual(github.api(path), {"status": "ahead"})
        self.assertEqual(request.call_args.args[0], "https://api.github.com" + path)

    def test_real_api_wrapper_rejects_literal_and_encoded_traversal_before_https(self):
        github = GitHub(secrets.token_hex(16))
        paths = [
            "/repos/x/../other",
            "/repos/x/./other",
            "/repos/x/%2e%2e/other",
            "/repos/x/%2e/other",
            "/repos/x/%2E%2e%2fother",
            "/repos/x/%5c..%5cother",
            "/repos/x/%00other",
            "/repos/x/%0aother",
            "/repos/x/%7fother",
            "/repos/x/\tother",
            "/repos/x/\nother",
            "//example.com/path",
            "/repos/x#fragment",
            "https://example.com/path",
        ]
        for path in paths:
            with mock.patch("api.fetch") as request, self.subTest(
                path=path
            ), self.assertRaises(PolicyError):
                github.api(path)
            request.assert_not_called()

    def test_known_git_blob_identity_is_protocol_bound(self):
        self.assertEqual(
            object_id(b"hello\n"), "ce013625030ba8dba906f756967f9e9ca394464a"
        )

    def test_github_redirect_never_forwards_credentials(self):
        request = urllib.request.Request(
            "https://api.github.com/example",
            headers={"Authorization": "Bearer " + secrets.token_hex(16)},
        )
        with self.assertRaises(PolicyError):
            NoRedirect().redirect_request(
                request, None, 302, "redirect", {}, "https://example.com"
            )

    def test_official_docker_cdn_redirect_strips_every_credential(self):
        request = urllib.request.Request(
            "https://registry-1.docker.io/v2/library/rust/blobs/sha256:" + "a" * 64,
            headers={
                "Authorization": "Bearer " + secrets.token_hex(16),
                "Cookie": secrets.token_hex(16),
            },
        )
        redirected = DockerBlobRedirect().redirect_request(
            request,
            None,
            307,
            "redirect",
            {},
            "https://production.cloudfront.docker.com/public-blob",
        )
        self.assertFalse(redirected.has_header("Authorization"))
        self.assertFalse(redirected.has_header("Cookie"))
        for url in [
            "http://production.cloudfront.docker.com/blob",
            "https://example.com/blob",
            "https://production.cloudfront.docker.com:444/blob",
            "https://user@production.cloudfront.docker.com/blob",
        ]:
            with self.subTest(url=url), self.assertRaises(PolicyError):
                DockerBlobRedirect().redirect_request(
                    request, None, 307, "redirect", {}, url
                )

    def test_git_blob_bytes_size_and_tree_identity_are_verified(self):
        data = b"compiler policy\n"
        identity = object_id(data)
        github = GitHub(None)
        blob = {
            "sha": identity,
            "encoding": "base64",
            "content": base64.b64encode(data).decode(),
            "size": len(data),
        }
        entry = {"sha": identity, "mode": "100644", "type": "blob"}
        with mock.patch.object(github, "api", return_value=blob):
            self.assertEqual(github.blob("ThreatFlux/example", entry), data)
        for key, value in [
            ("sha", "f" * 40),
            ("size", len(data) + 1),
            ("content", base64.b64encode(b"changed").decode()),
        ]:
            with mock.patch.object(
                github, "api", return_value=dict(blob, **{key: value})
            ), self.assertRaises(PolicyError):
                github.blob("ThreatFlux/example", entry)
        for mode in ["120000", "160000"]:
            with self.assertRaises(PolicyError):
                github.blob("ThreatFlux/example", dict(entry, mode=mode))

    def test_truncated_git_tree_is_not_an_authoritative_input(self):
        github = GitHub(None)
        responses = [
            {"sha": fixtures.SOURCE, "tree": {"sha": fixtures.TREE}},
            {"sha": fixtures.TREE, "truncated": True, "tree": []},
        ]
        with mock.patch.object(github, "api", side_effect=responses), self.assertRaises(
            PolicyError
        ):
            github.tree("ThreatFlux/example", fixtures.SOURCE)

    def channel_fixture(self, prerelease=False):
        body = b'manifest-version="2"\ndate="2026-11-12"\n[pkg.rust]\nversion="1.101.0 (abcdef 2026-11-12)"\n'
        checksum = (
            hashlib.sha256(body).hexdigest().encode() + b"  channel-rust-stable.toml"
        )
        github = mock.Mock()
        github.api.return_value = {
            "draft": False,
            "prerelease": prerelease,
            "tag_name": "1.101.0",
            "html_url": "https://github.com/rust-lang/rust/releases/tag/1.101.0",
            "published_at": "2026-11-12T00:00:00Z",
        }
        return github, [(body, {}), (checksum, {})]

    def test_stable_channel_requires_primary_checksum_and_nondraft_release(self):
        github, responses = self.channel_fixture()
        with mock.patch("targets.fetch", side_effect=responses):
            self.assertEqual(stable_channel(github)["version"], "1.101.0")
        github, responses = self.channel_fixture(prerelease=True)
        with mock.patch("targets.fetch", side_effect=responses), self.assertRaises(
            PolicyError
        ):
            stable_channel(github)
        github, responses = self.channel_fixture()
        responses[-1] = (b"0" * 64, {})
        with mock.patch("targets.fetch", side_effect=responses), self.assertRaises(
            PolicyError
        ):
            stable_channel(github)

    def image_fixture(self, architecture="arm64"):
        return {
            "manifests": [
                {
                    "digest": "sha256:" + letter * 64,
                    "size": 10,
                    "platform": {"os": "linux", "architecture": arch},
                }
                for letter, arch in [("a", "amd64"), ("b", architecture)]
            ]
        }

    def test_both_official_image_architectures_are_required(self):
        registry = object.__new__(RustImages)
        with mock.patch.object(
            registry,
            "object",
            return_value=(self.image_fixture(), "sha256:" + "c" * 64),
        ), mock.patch.object(
            registry,
            "platform",
            side_effect=lambda item, version: {
                "platform": "linux/" + item["platform"]["architecture"]
            },
        ):
            result = registry.image("1.101.0", "trixie")
            self.assertEqual(
                [item["platform"] for item in result["platforms"]],
                ["linux/amd64", "linux/arm64"],
            )
        with mock.patch.object(
            registry,
            "object",
            return_value=(self.image_fixture("amd64"), "sha256:" + "c" * 64),
        ), self.assertRaises(PolicyError):
            registry.image("1.101.0", "trixie")

    def test_official_config_must_match_compiler_and_platform(self):
        registry = object.__new__(RustImages)
        descriptor = {
            "digest": "sha256:" + "a" * 64,
            "size": 10,
            "platform": {"architecture": "arm64"},
        }
        manifest = {"config": {"digest": "sha256:" + "b" * 64, "size": 20}}
        for config in [
            {
                "os": "linux",
                "architecture": "amd64",
                "config": {"Env": ["RUST_VERSION=1.101.0"]},
            },
            {
                "os": "linux",
                "architecture": "arm64",
                "config": {"Env": ["RUST_VERSION=1.100.0"]},
            },
        ]:
            with mock.patch.object(
                registry,
                "object",
                side_effect=[
                    (manifest, descriptor["digest"]),
                    (config, manifest["config"]["digest"]),
                ],
            ), self.assertRaises(PolicyError):
                registry.platform(descriptor, "1.101.0")

    def test_registry_descriptor_digest_and_size_are_enforced(self):
        registry = object.__new__(RustImages)
        registry.token = secrets.token_hex(16)
        body = json.dumps({"config": {}}).encode()
        digest = "sha256:" + hashlib.sha256(body).hexdigest()
        with mock.patch(
            "targets.fetch", return_value=(body, {"Docker-Content-Digest": digest})
        ):
            self.assertEqual(registry.object("manifests", digest, len(body))[1], digest)
        with mock.patch("targets.fetch", return_value=(body, {})), self.assertRaises(
            PolicyError
        ):
            registry.object("manifests", "sha256:" + "f" * 64)
        with mock.patch("targets.fetch", return_value=(body, {})), self.assertRaises(
            PolicyError
        ):
            registry.object("manifests", digest, len(body) + 1)
