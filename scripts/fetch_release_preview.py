"""Fetch only verified Cargo metadata for a release preview, never candidate code."""

import argparse
import base64
import binascii
import hashlib
import http.client
import json
import os
import re
import ssl
import urllib.parse
from pathlib import Path, PurePosixPath, PureWindowsPath

CARGO_FILES = {"Cargo.toml", "Cargo.lock"}
REGULAR_MODES = {"100644", "100755"}


class PreviewError(ValueError):
    """Unverified or unsafe preview data must not reach the analyzer."""


def object_sha(value):
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{40}", value) is None:
        raise PreviewError("GitHub returned an invalid Git object identity")
    return value


def relative_path(value):
    if not isinstance(value, str) or not value or "\\" in value or "\0" in value:
        raise PreviewError("Git tree paths must be nonempty relative POSIX paths")
    parts = value.split("/")
    if PurePosixPath(value).is_absolute() or PureWindowsPath(value).drive:
        raise PreviewError("Absolute or drive-qualified Git tree paths are forbidden")
    if any(
        part in {"", ".", ".."} or part.rstrip(" .").lower() == ".git" for part in parts
    ):
        raise PreviewError("Git tree paths cannot traverse or create Git metadata")
    return PurePosixPath(value)


class GitHubApi:
    def __init__(self, base_url, token):
        try:
            base = urllib.parse.urlsplit(base_url)
            valid = (
                base.scheme == "https"
                and base.hostname
                and not (base.username or base.password or base.query or base.fragment)
            )
            if not valid or not token:
                raise PreviewError(
                    "A trusted HTTPS GitHub API URL and GH_TOKEN are required"
                )
            self.host, self.port = base.hostname, base.port
        except ValueError as error:
            raise PreviewError("Invalid HTTPS GitHub API configuration") from error
        self.prefix = base.path.rstrip("/")
        self.headers = {
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "User-Agent": "ThreatFlux-release-preview",
        }

    def get(self, route):
        connection = http.client.HTTPSConnection(
            self.host, self.port, timeout=30, context=ssl.create_default_context()
        )
        try:
            connection.request("GET", self.prefix + route, headers=self.headers)
            response = connection.getresponse()
            if response.status != 200:
                # HTTPSConnection does not follow redirects or forward credentials.
                raise PreviewError(f"GitHub API returned HTTP {response.status}")
            payload = json.loads(response.read())
        except PreviewError:
            raise
        except (OSError, http.client.HTTPException, ValueError):
            raise PreviewError("GitHub API request failed") from None
        finally:
            connection.close()
        if not isinstance(payload, dict):
            raise PreviewError("GitHub API returned a non-object response")
        return payload


def source_tree(api, repository, sha):
    if re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) is None:
        raise PreviewError("Expected a GitHub owner/repository name")
    if any(part in {".", ".."} for part in repository.split("/")):
        raise PreviewError("Repository coordinates cannot traverse API paths")
    sha = object_sha(sha)
    commit = api.get(f"/repos/{repository}/git/commits/{sha}")
    if commit.get("sha") != sha:
        raise PreviewError("GitHub commit response does not match the requested SHA")
    tree_sha = object_sha(commit.get("tree", {}).get("sha"))
    tree = api.get(f"/repos/{repository}/git/trees/{tree_sha}?recursive=1")
    if tree.get("sha") != tree_sha or tree.get("truncated") is not False:
        raise PreviewError("GitHub tree identity is wrong or its history is truncated")
    return selected_entries(tree.get("tree", []))


def selected_entries(entries):
    catalog = {}
    for entry in entries:
        path = relative_path(entry.get("path"))
        if path in catalog:
            raise PreviewError("GitHub tree contains a duplicate path")
        catalog[path] = entry
    selected = []
    for path, entry in catalog.items():
        if path.name not in CARGO_FILES:
            continue
        if entry.get("type") != "blob" or entry.get("mode") not in REGULAR_MODES:
            raise PreviewError("Selected Cargo files cannot be symlinks or submodules")
        object_sha(entry.get("sha"))
        if type(entry.get("size")) is not int or entry["size"] < 0:
            raise PreviewError("Selected Cargo file has an invalid size")
        for parent in path.parents:
            if parent == PurePosixPath("."):
                continue
            directory = catalog.get(parent, {})
            if directory.get("type") != "tree" or directory.get("mode") != "040000":
                raise PreviewError(
                    "A Cargo file's parent is not a regular Git directory"
                )
        selected.append(entry)
    if PurePosixPath("Cargo.toml") not in {
        PurePosixPath(entry["path"]) for entry in selected
    }:
        raise PreviewError("The selected commit has no regular root Cargo.toml")
    return sorted(selected, key=lambda entry: entry["path"])


def read_blob(api, repository, entry):
    blob = api.get(f"/repos/{repository}/git/blobs/{entry['sha']}")
    if blob.get("sha") != entry["sha"] or blob.get("encoding") != "base64":
        raise PreviewError("GitHub blob identity or encoding does not match the tree")
    try:
        data = base64.b64decode("".join(blob["content"].split()), validate=True)
    except (KeyError, AttributeError, binascii.Error, ValueError):
        raise PreviewError("GitHub blob contains invalid base64 data") from None
    if (
        type(blob.get("size")) is not int
        or blob["size"] != len(data)
        or entry["size"] != len(data)
    ):
        raise PreviewError("GitHub blob byte size does not match its declared size")
    # Git object IDs require SHA-1 framing; HTTPS and the pinned commit provide trust.
    framed = b"blob " + str(len(data)).encode("ascii") + b"\0" + data
    if hashlib.sha1(framed, usedforsecurity=False).hexdigest() != entry["sha"]:
        raise PreviewError("Git blob content hash does not match its object identity")
    return data


def fresh_output(workspace, name):
    relative = relative_path(str(name))
    if len(relative.parts) != 1:
        raise PreviewError("Preview output must be a fresh direct workspace child")
    workspace = workspace.resolve()
    output = workspace.joinpath(*relative.parts)
    if not output.resolve().is_relative_to(workspace):
        raise PreviewError("Preview output escapes the workspace")
    output.mkdir(mode=0o700)
    return output


def fetch_preview(api, repository, sha, output, workspace):
    entries = source_tree(api, repository, sha)
    destination = fresh_output(workspace, output)
    for entry in entries:
        data = read_blob(api, repository, entry)
        target = destination.joinpath(*relative_path(entry["path"]).parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        if not target.resolve().is_relative_to(destination.resolve()):
            raise PreviewError("Cargo output escapes the fresh preview directory")
        with target.open("xb") as stream:
            stream.write(data)
    return len(entries)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--output", required=True)
    arguments = parser.parse_args(argv)
    api = GitHubApi(
        os.environ.get("GITHUB_API_URL", "https://api.github.com"),
        os.environ.get("GH_TOKEN", ""),
    )
    count = fetch_preview(
        api, arguments.repository, arguments.sha, arguments.output, Path.cwd()
    )
    print(
        f"Fetched {count} verified Cargo data files from {arguments.repository}@{arguments.sha}"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (PreviewError, OSError) as error:
        raise SystemExit(f"Release preview data unavailable: {error}") from None
