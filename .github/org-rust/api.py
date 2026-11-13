"""Read immutable Git data and make narrowly scoped GitHub API requests."""

import base64
import hashlib
import json
import re
import urllib.error
import urllib.request
from urllib.parse import unquote, urlsplit


class PolicyError(RuntimeError):
    """Stop the rollout before accepting an ambiguous or unverifiable state."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise PolicyError("HTTP redirects are not accepted")


class DockerBlobRedirect(urllib.request.HTTPRedirectHandler):
    max_redirections = 3

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        target = urlsplit(newurl)
        allowed = {
            "production.cloudfront.docker.com",
            "production.cloudflare.docker.com",
        }
        if (
            target.scheme != "https"
            or target.hostname not in allowed
            or target.port not in {None, 443}
            or target.username
        ):
            raise PolicyError(
                "Official Docker blob redirect left the approved CDN boundary"
            )
        # Docker's public blob CDN needs no registry authorization or cookies.
        return urllib.request.Request(
            newurl,
            headers={
                "Accept": req.get_header("Accept", "application/json"),
                "User-Agent": "ThreatFlux-org-rust-policy",
            },
        )


def fetch(url, token=None, payload=None, accept="application/json", public_blob=False):
    if not url.startswith("https://"):
        raise PolicyError("Only HTTPS requests are supported")
    headers = {"Accept": accept, "User-Agent": "ThreatFlux-org-rust-policy"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = None if payload is None else json.dumps(payload).encode()
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers)
    if public_blob and not url.startswith(
        "https://registry-1.docker.io/v2/library/rust/blobs/sha256:"
    ):
        raise PolicyError("Public CDN handling is restricted to official Rust blobs")
    opener = urllib.request.build_opener(
        DockerBlobRedirect() if public_blob else NoRedirect()
    )
    try:
        with opener.open(request, timeout=60) as response:
            return response.read(), dict(response.headers)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None, {"status": 404}
        raise PolicyError(f"HTTPS request failed with status {error.code}") from error


def object_id(data):
    framed = b"blob " + str(len(data)).encode("ascii") + b"\0" + data
    # Git object identity uses SHA-1 framing; this is not a security hash choice.
    return hashlib.sha1(  # nosemgrep: insecure-hash-algorithm-sha1
        framed, usedforsecurity=False
    ).hexdigest()


def sha(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{40}", value):
        raise PolicyError("Invalid Git object identity")
    return value


def safe_path(value):
    if (
        not isinstance(value, str)
        or not value
        or value.startswith("/")
        or "\\" in value
        or any(part in {"", ".", ".."} for part in value.split("/"))
    ):
        raise PolicyError("Invalid repository-relative policy path")
    return value


def control_characters(value):
    return any(ord(character) < 32 or ord(character) == 127 for character in value)


def validate_api_path(path):
    if not isinstance(path, str) or control_characters(path):
        raise PolicyError("Invalid GitHub API path")
    parsed = urlsplit(path)
    decoded = unquote(parsed.path)
    if (
        not path.startswith("/")
        or parsed.scheme
        or parsed.netloc
        or parsed.fragment
        or decoded.startswith("//")
        or "\\" in decoded
        or control_characters(decoded)
        or any(part in {".", ".."} for part in decoded.split("/"))
    ):
        raise PolicyError("Invalid GitHub API path")


class GitHub:
    def __init__(self, token):
        self.token = token

    def api(self, path, payload=None, missing=False):
        validate_api_path(path)
        body, _ = fetch("https://api.github.com" + path, self.token, payload)
        if body is None:
            if missing:
                return None
            raise PolicyError("Required GitHub API object was not found")
        return json.loads(body)

    def graphql(self, query, variables):
        result = self.api("/graphql", {"query": query, "variables": variables})
        if result.get("errors"):
            raise PolicyError("GitHub rejected the expected-head mutation")
        return result["data"]

    def pages(self, path, key=None):
        rows = []
        for page in range(1, 11):
            separator = "&" if "?" in path else "?"
            result = self.api(f"{path}{separator}per_page=100&page={page}")
            batch = result[key] if key else result
            rows.extend(batch)
            if len(batch) < 100:
                if key and result.get("total_count") != len(rows):
                    raise PolicyError("Incomplete GitHub pagination")
                return rows
        raise PolicyError("GitHub pagination safety limit reached")

    def tree(self, repository, commit):
        commit = sha(commit)
        obj = self.api(f"/repos/{repository}/git/commits/{commit}")
        if obj["sha"] != commit:
            raise PolicyError("Commit identity mismatch")
        tree = self.api(
            f"/repos/{repository}/git/trees/{sha(obj['tree']['sha'])}?recursive=1"
        )
        if tree.get("truncated") or tree["sha"] != obj["tree"]["sha"]:
            raise PolicyError("Incomplete immutable Git tree")
        return tree["sha"], {entry["path"]: entry for entry in tree["tree"]}

    def blob(self, repository, entry):
        if entry["type"] != "blob" or entry["mode"] not in {"100644", "100755"}:
            raise PolicyError("Selected policy input must be a regular Git file")
        obj = self.api(f"/repos/{repository}/git/blobs/{sha(entry['sha'])}")
        if obj["sha"] != entry["sha"] or obj["encoding"] != "base64":
            raise PolicyError("Blob metadata differs from the immutable tree")
        data = base64.b64decode(obj["content"], validate=False)
        if len(data) != obj["size"] or object_id(data) != entry["sha"]:
            raise PolicyError("Git blob bytes differ from the immutable tree")
        return data
