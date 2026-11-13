"""Public synthetic API histories and representative maintained Rust layouts."""

import base64
import copy
import hashlib
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from api import PolicyError, object_id
from targets import VARIANTS

SOURCE = "a" * 40
TREE = "b" * 40
COMMIT = "c" * 40
CENTRAL_SOURCE = "d" * 40
REPOSITORY = "ThreatFlux/example"
BOT = "threatflux-automation[bot]"
CI = """name: CI
jobs:
  stable:
    steps:
      - name: Install Rust
        toolchain: 1.100.0
      - run: rustc --version
  msrv:
    steps:
      - name: Install Rust
        toolchain: 1.100.0
      - run: rustc --version
  compatibility:
    rust: [beta, nightly, '1.97.1']
"""


def target(version="1.100.0"):
    images = {}
    for variant in sorted(VARIANTS):
        digest = "sha256:" + hashlib.sha256((version + variant).encode()).hexdigest()
        images[variant] = {
            "reference": f"rust:{version}-{variant}@{digest}",
            "index_digest": digest,
            "platforms": [],
        }
    return {"stable": {"version": version, "date": "2026-11-12"}, "images": images}


def policy():
    return {
        "schema_version": 1,
        "organization": "ThreatFlux",
        "writes_enabled": True,
        "app": {"id": 5205173, "slug": "threatflux-automation"},
        "target": target(),
        "accepted_states": [],
        "repositories": [
            {
                "repository": REPOSITORY,
                "id": 123,
                "default_branch": "main",
                "files": [
                    {
                        "path": ".github/workflows/ci.yml",
                        "mode": "100644",
                        "adapters": [
                            {
                                "kind": "plain",
                                "job": "stable",
                                "count": 1,
                                "template": "      - name: Install Rust\n        toolchain: @@RUST_VALUE@@\n      - run: rustc --version\n",
                            }
                        ],
                    }
                ],
            }
        ],
    }


def upgraded(version="1.101.0"):
    value = policy()
    value["accepted_states"] = [value["target"]]
    value["target"] = target(version)
    return value


class FakeGitHub:
    def __init__(self, files=None):
        self.files = files or {
            ".github/workflows/ci.yml": CI.encode(),
            "Cargo.toml": b'[package]\nrust-version="1.100.0"\n',
        }
        self.source = SOURCE
        self.branch = None
        self.pulls = []
        self.writes = []
        self.reads = []
        self.permissions = {
            "contents": "write",
            "pull_requests": "write",
            "workflows": "write",
        }
        self.installed_ids = [123]
        self.verified = True
        self.author = BOT
        self.on_graphql = None
        self.on_source = None
        self.proposed_files = None
        self.central_source = CENTRAL_SOURCE
        self.on_ref_created = None

    def pages(self, path, key=None):
        if path == "/installation/repositories":
            return [{"id": identity} for identity in self.installed_ids]
        if "/pulls?" in path:
            return copy.deepcopy(self.pulls)
        raise AssertionError(path)

    def tree(self, repository, source):
        files = (
            self.proposed_files
            if source == COMMIT and self.proposed_files is not None
            else self.files
        )
        entries = {
            name: {
                "path": name,
                "sha": object_id(data),
                "mode": "100644",
                "type": "blob",
            }
            for name, data in files.items()
        }
        for name in list(entries):
            for index in range(1, len(name.split("/"))):
                parent = "/".join(name.split("/")[:index])
                entries[parent] = {
                    "path": parent,
                    "sha": TREE,
                    "mode": "040000",
                    "type": "tree",
                }
        return TREE, entries

    def blob(self, repository, entry):
        return self.files[entry["path"]]

    def api(self, path, payload=None, missing=False):
        self.reads.append(path)
        if payload is not None:
            return self.mutate(path, payload)
        if path == "/apps/threatflux-automation":
            return {
                "id": 5205173,
                "slug": "threatflux-automation",
                "owner": {"login": "ThreatFlux"},
                "permissions": self.permissions,
            }
        if path == "/repos/" + REPOSITORY:
            return {
                "id": 123,
                "full_name": REPOSITORY,
                "default_branch": "main",
                "private": False,
                "archived": False,
                "fork": False,
            }
        if path.endswith("/git/ref/heads/main"):
            if "github_actions" in path:
                return {"object": {"sha": self.central_source}}
            if self.on_source:
                self.on_source(self)
            return {
                "ref": "refs/heads/main",
                "object": {"sha": self.source, "type": "commit"},
            }
        if "/git/ref/heads/codex/org-rust-" in path:
            return None if self.branch is None else {"object": {"sha": self.branch}}
        if path.endswith("/commits/" + COMMIT):
            return {
                "author": {"login": self.author},
                "parents": [{"sha": self.source}],
                "commit": {"verification": {"verified": self.verified}},
            }
        if "/compare/" in path:
            return {
                "status": "ahead",
                "files": [
                    {"filename": ".github/workflows/ci.yml", "sha": self.proposed_sha}
                ],
            }
        raise AssertionError(path)

    def mutate(self, path, payload):
        self.writes.append((path, payload))
        if path.endswith("/git/refs"):
            self.branch = payload["sha"]
            if self.on_ref_created:
                self.on_ref_created(self)
            return {"object": {"sha": self.branch}}
        if path.endswith("/pulls"):
            pull = {
                "html_url": "https://github.com/ThreatFlux/example/pull/1",
                "user": {"login": BOT},
                "head": {"ref": payload["head"], "repo": {"id": 123}, "sha": COMMIT},
            }
            self.pulls = [pull]
            return pull
        raise AssertionError(path)

    def graphql(self, query, variables):
        if self.on_graphql:
            self.on_graphql(self)
        data = variables["input"]
        if data["expectedHeadOid"] != self.branch:
            raise PolicyError("Expected branch head moved")
        self.writes.append(("graphql", variables))
        self.branch = COMMIT
        content = base64.b64decode(data["fileChanges"]["additions"][0]["contents"])
        self.proposed_sha = object_id(content)
        self.proposed_files = dict(self.files)
        for addition in data["fileChanges"]["additions"]:
            self.proposed_files[addition["path"]] = base64.b64decode(
                addition["contents"]
            )
        return {"createCommitOnBranch": {"commit": {"oid": COMMIT}}}
