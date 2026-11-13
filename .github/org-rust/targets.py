"""Resolve the official stable compiler and matching immutable Rust images."""

import hashlib
import json
import re
import tomllib

from api import PolicyError, fetch

CHANNEL = "https://static.rust-lang.org/dist/channel-rust-stable.toml"
REGISTRY = "https://registry-1.docker.io/v2/library/rust/"
ACCEPT = ", ".join(
    [
        "application/vnd.oci.image.index.v1+json",
        "application/vnd.docker.distribution.manifest.list.v2+json",
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.v2+json",
    ]
)
PLATFORMS = {"linux/amd64", "linux/arm64"}
VARIANTS = {"alpine3.24", "trixie", "slim-trixie", "bookworm"}


def stable_channel(github):
    data, _ = fetch(CHANNEL, accept="application/toml")
    checksum, _ = fetch(CHANNEL + ".sha256", accept="text/plain")
    digest = hashlib.sha256(data).hexdigest()
    if checksum is None or checksum.decode().split()[0] != digest:
        raise PolicyError("Official stable channel checksum differs")
    channel = tomllib.loads(data.decode())
    version = channel["pkg"]["rust"]["version"].split()[0]
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise PolicyError("Official channel is not a stable Rust version")
    release = github.api(f"/repos/rust-lang/rust/releases/tags/{version}")
    if release["draft"] or release["prerelease"] or release["tag_name"] != version:
        raise PolicyError("Official Rust release is not stable")
    return {
        "version": version,
        "date": str(channel["date"]),
        "manifest_url": CHANNEL,
        "manifest_sha256": digest,
        "release_url": release["html_url"],
        "published_at": release["published_at"],
    }


class RustImages:
    def __init__(self):
        body, _ = fetch(
            "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/rust:pull"
        )
        self.token = json.loads(body)["token"]

    def object(self, kind, reference, size=None):
        body, headers = fetch(
            REGISTRY + kind + "/" + reference,
            self.token,
            accept=ACCEPT,
            public_blob=kind == "blobs",
        )
        if body is None:
            raise PolicyError("A matching official Rust image is unavailable")
        digest = "sha256:" + hashlib.sha256(body).hexdigest()
        declared = headers.get("Docker-Content-Digest")
        if (declared and declared != digest) or (
            reference.startswith("sha256:") and reference != digest
        ):
            raise PolicyError("Official image content digest differs")
        if size is not None and len(body) != size:
            raise PolicyError("Official image descriptor byte size differs")
        return json.loads(body), digest

    def platform(self, descriptor, version):
        manifest, digest = self.object(
            "manifests", descriptor["digest"], descriptor["size"]
        )
        config_ref = manifest["config"]
        config, config_digest = self.object(
            "blobs", config_ref["digest"], config_ref["size"]
        )
        platform = descriptor["platform"]
        if (
            config["os"] != "linux"
            or config["architecture"] != platform["architecture"]
        ):
            raise PolicyError("Official image platform configuration differs")
        compiler_values = [
            value
            for value in config.get("config", {}).get("Env", [])
            if value.startswith("RUST_VERSION=")
        ]
        if compiler_values != [f"RUST_VERSION={version}"]:
            raise PolicyError("Official image declares a different Rust compiler")
        return {
            "platform": "linux/" + platform["architecture"],
            "manifest_digest": digest,
            "config_digest": config_digest,
        }

    def image(self, version, variant):
        if variant not in VARIANTS:
            raise PolicyError("Unsupported official Rust image variant")
        tag = version + "-" + variant
        index, digest = self.object("manifests", tag)
        selected = [
            item
            for item in index.get("manifests", [])
            if item.get("platform", {}).get("os", "")
            + "/"
            + item.get("platform", {}).get("architecture", "")
            in PLATFORMS
        ]
        identities = [item["platform"]["architecture"] for item in selected]
        if sorted(identities) != ["amd64", "arm64"]:
            raise PolicyError(
                "Official image must resolve both supported architectures"
            )
        return {
            "reference": "rust:" + tag + "@" + digest,
            "index_digest": digest,
            "platforms": sorted(
                [self.platform(item, version) for item in selected],
                key=lambda item: item["platform"],
            ),
        }


def official_target(github):
    stable = stable_channel(github)
    registry = RustImages()
    return {
        "stable": stable,
        "images": {
            variant: registry.image(stable["version"], variant)
            for variant in sorted(VARIANTS)
        },
    }


def validate_target(policy, actual):
    if policy["target"]["stable"] != actual["stable"]:
        raise PolicyError(
            "Reviewed compiler policy differs from current official stable metadata"
        )
    if policy["target"]["images"] != actual["images"]:
        raise PolicyError(
            "Reviewed compiler/image policy differs from current official images"
        )
