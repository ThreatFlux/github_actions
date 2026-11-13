"""Exercise sequential Rust upgrades while retaining compatibility identities."""

import copy
import json
import unittest
from pathlib import Path

from fixtures import CI, policy, target, upgraded
from adapters import patch_file, render, validate_file
from api import PolicyError
from control import select_repositories, validate_policy

TRIMMED_ASSERTIONS = {
    ("ThreatFlux/file-scanner", "Dockerfile"),
    ("ThreatFlux/github_actions", "Dockerfile"),
    ("ThreatFlux/ollama_rust_sdk", "Dockerfile"),
    ("ThreatFlux/openai_rust_sdk", "Dockerfile"),
    ("ThreatFlux/rust-cicd-template", "Dockerfile"),
    ("ThreatFlux/rust-cicd-template", "Dockerfile.debian"),
    ("ThreatFlux/vertex_rust_sdk", "Dockerfile"),
    ("ThreatFlux/virustotal-rs", "Dockerfile"),
    ("ThreatFlux/yrlint", "Dockerfile"),
}


class AdapterTests(unittest.TestCase):
    def test_all_real_compiler_assertions_ignore_independent_product_arg_versions(self):
        catalog = json.loads(
            Path(__file__).resolve().parents[1].joinpath("policy.json").read_text()
        )
        assertions = [
            adapter
            for repository in catalog["repositories"]
            for file in repository["files"]
            for adapter in file["adapters"]
            if (repository["repository"], file["path"]) in TRIMMED_ASSERTIONS
            and "rustfmt" in adapter["template"]
            and any(
                anchor in adapter["template"]
                for anchor in [
                    "'^rustc @@RUST_VALUE@@ '",
                    "= @@RUST_VALUE@@\n",
                    '= "@@RUST_VALUE@@"\n',
                ]
            )
        ]
        self.assertEqual(len(assertions), 9)
        suffix = "\nARG VERSION=9.8.7\nARG BUILD_DATE=2026-11-13\n"
        for adapter in assertions:
            context = adapter["template"].replace("@@RUST_plain@@", "1.100.0")
            self.assertNotIn("ARG VERSION", context)
            original = (
                context.replace("@@RUST_VALUE@@", render("1.100.0", adapter["kind"]))
                + suffix
            )
            expected = (
                context.replace("@@RUST_VALUE@@", render("1.101.0", adapter["kind"]))
                + suffix
            )
            with self.subTest(template=adapter["template"]):
                self.assertEqual(patch_file(original, [adapter], upgraded()), expected)
                with self.assertRaises(PolicyError):
                    patch_file(
                        original.replace("rustfmt", "unexpected-component"),
                        [adapter],
                        upgraded(),
                    )

    def test_same_number_msrv_beta_nightly_and_cargo_floor_stay_unchanged(self):
        config = upgraded()
        adapters = config["repositories"][0]["files"][0]["adapters"]
        result = patch_file(CI, adapters, config)
        self.assertIn(
            "  stable:\n    steps:\n      - name: Install Rust\n        toolchain: 1.101.0",
            result,
        )
        self.assertIn(
            "  msrv:\n    steps:\n      - name: Install Rust\n        toolchain: 1.100.0",
            result,
        )
        self.assertIn("rust: [beta, nightly, '1.97.1']", result)
        with self.assertRaises(PolicyError):
            validate_file("Cargo.toml")

    def test_sequential_upgrades_use_same_reviewed_context_then_are_idempotent(self):
        config = upgraded()
        adapters = config["repositories"][0]["files"][0]["adapters"]
        first = patch_file(CI, adapters, config)
        self.assertEqual(first, patch_file(first, adapters, config))
        config["accepted_states"].append(config["target"])
        config["target"] = target("1.102.0")
        second = patch_file(first, adapters, config)
        self.assertIn("toolchain: 1.102.0", second)
        self.assertIn("toolchain: 1.100.0", second)
        self.assertEqual(second, patch_file(second, adapters, config))

    def test_coupled_image_and_compiler_slots_change_from_same_original_bytes(self):
        config = upgraded()
        old = config["accepted_states"][0]["images"]["alpine3.24"]["reference"]
        image = {
            "kind": "image:alpine3.24",
            "count": 1,
            "template": "FROM @@RUST_VALUE@@ AS builder\nENV RUST_VERSION=@@RUST_plain@@\n",
        }
        version = {
            "kind": "plain",
            "count": 1,
            "template": "FROM @@IMAGE_alpine3.24@@ AS builder\nENV RUST_VERSION=@@RUST_VALUE@@\n",
        }
        original = f"FROM {old} AS builder\nENV RUST_VERSION=1.100.0\n"
        result = patch_file(original, [image, version], config)
        self.assertEqual(
            result,
            f"FROM {config['target']['images']['alpine3.24']['reference']} AS builder\nENV RUST_VERSION=1.101.0\n",
        )

    def test_unknown_or_duplicate_context_fails_closed(self):
        config = upgraded()
        adapters = config["repositories"][0]["files"][0]["adapters"]
        for text in [
            CI.replace("Install Rust", "Different layout"),
            CI.replace("toolchain: 1.100.0", "toolchain: 1.103.0"),
            CI.replace(
                "      - run: rustc --version",
                "      - run: rustc --version\n      - name: Install Rust\n        toolchain: 1.100.0\n      - run: rustc --version",
                1,
            ),
        ]:
            with self.subTest(text=text), self.assertRaises(PolicyError):
                patch_file(text, adapters, config)

    def test_regex_compiler_assertion_renders_dots_without_touching_other_text(self):
        config = upgraded()
        for style, old, new in [
            ("escaped", r"1\.100\.0", r"1\.101\.0"),
            ("bracketed", "1[.]100[.]0", "1[.]101[.]0"),
        ]:
            adapter = {
                "kind": style,
                "count": 1,
                "template": "grep '^rustc @@RUST_VALUE@@ '\n",
            }
            self.assertEqual(
                patch_file(f"grep '^rustc {old} '\n", [adapter], config),
                f"grep '^rustc {new} '\n",
            )

    def test_fixture_vendor_legacy_and_lock_paths_are_not_manageable(self):
        for path in [
            "tests/msrv/rust-toolchain.toml",
            "vendor/Dockerfile",
            "fuzz/Makefile",
            "legacy/Dockerfile",
            "fixtures/ci.yml",
            "Cargo.lock",
            "../Dockerfile",
            "/Dockerfile",
            "a\\Dockerfile",
        ]:
            with self.subTest(path=path), self.assertRaises(PolicyError):
                validate_file(path)

    def test_unknown_subset_and_other_organizations_are_default_denied(self):
        config = policy()
        for organization, subset in [("Elsewhere", ""), ("ThreatFlux", "unlisted")]:
            with self.assertRaises(PolicyError):
                select_repositories(config, organization, subset)
        self.assertEqual(len(select_repositories(config, "ThreatFlux", "example")), 1)

    def test_duplicate_ids_or_nonboolean_activation_fail_closed(self):
        config = policy()
        config["repositories"].append(copy.deepcopy(config["repositories"][0]))
        with self.assertRaises(PolicyError):
            validate_policy(config)
        config = policy()
        config["writes_enabled"] = "false"
        with self.assertRaises(PolicyError):
            validate_policy(config)

    def test_downgrade_is_rejected(self):
        config = upgraded("1.99.0")
        with self.assertRaises(PolicyError):
            patch_file(CI, config["repositories"][0]["files"][0]["adapters"], config)
