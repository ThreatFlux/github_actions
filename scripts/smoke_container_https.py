#!/usr/bin/env python3
"""Exercise GitHub Actions Maintainer CLI hostname lookup and certificate validation in its image."""

import argparse
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import ssl
# Fixed Docker/OpenSSL executables use argv arrays, never a shell.
import subprocess  # nosec B404
import tempfile
import threading


class Handler(BaseHTTPRequestHandler):
    """Serve the fixed commit fixture without accepting mutable API operations."""

    def do_GET(self):
        """Serve one fabricated GitHub commit response for the TLS fixture."""
        if self.path != "/repos/fixture/action/commits/v1":
            self.send_error(404)
            return
        self.server.requests.append(self.path)
        response = b'{"sha":"1111111111111111111111111111111111111111"}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    def log_message(self, *_):
        """Keep the fixture quiet during expected certificate failures."""
        pass


def create_certificate(root):
    """Create a temporary CA and a leaf certificate for the fixture hostname."""
    certificate, key = root / "ca.crt", root / "key.pem"
    commands = [
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
         "-subj", "/CN=Temporary fixture CA", "-addext", "basicConstraints=critical,CA:TRUE",
         "-keyout", str(root / "ca.key"), "-out", str(certificate)],
        ["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
         "-subj", "/CN=host.docker.internal", "-keyout", str(key), "-out", str(root / "leaf.csr")],
        ["openssl", "x509", "-req", "-days", "1", "-in", str(root / "leaf.csr"),
         "-CA", str(certificate), "-CAkey", str(root / "ca.key"), "-CAcreateserial",
         "-extfile", str(root / "extensions.cnf"), "-out", str(root / "leaf.crt")],
    ]
    (root / "extensions.cnf").write_text(
        "basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:host.docker.internal\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")
    # Fixed OpenSSL subcommands; paths come from our private TemporaryDirectory.
    for command in commands:
        subprocess.run(  # nosec B603  # nosemgrep: dangerous-subprocess-use-audit
            command, check=True, shell=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
    certificate.chmod(0o644)
    return certificate, key


def run_cli(image, platform, port, root, certificate=None, hostname="host.docker.internal"):
    """Run the image with read-only fixture mounts and a temporary API endpoint."""
    command = ["docker", "run", "--rm", "--read-only", "--cap-drop", "ALL",
               "--security-opt", "no-new-privileges", "--add-host", f"{hostname}:host-gateway"]
    if platform:
        command += ["--platform", platform]
    if certificate:
        command += ["-v", f"{certificate}:/fixture/ca.crt:ro", "-e", "SSL_CERT_FILE=/fixture/ca.crt"]
    command += ["-v", f"{root}/repo:/fixture/repo:ro",
                "-e", f"GITHUB_API_BASE_URL=https://{hostname}:{port}"]
    # Validated image follows --; mounts, environment and CLI flags are fixed
    # fixture data in separate argv elements. No shell interprets any value.
    return subprocess.run(  # nosec B603  # nosemgrep: dangerous-subprocess-use-audit, dangerous-subprocess-use-tainted-env-args
        command + ["--", image, "pin", "--repo", "/fixture/repo", "--dry-run"],  # nosemgrep: dangerous-subprocess-use-tainted-env-args
        capture_output=True, text=True, timeout=45, shell=False,
    )


def check_tls(image, platform, server, certificate, root):
    """Require trusted HTTPS and reject unknown issuers and wrong hostnames."""
    trusted = run_cli(image, platform, server.server_port, root, certificate)
    if (trusted.returncode or "Dry run: would pin 1 action" not in trusted.stdout
        or "1111111111111111111111111111111111111111" not in trusted.stdout
        or len(server.requests) != 1):
        raise RuntimeError(f"trusted HTTPS failed: {trusted.stdout}\n{trusted.stderr}")
    untrusted = run_cli(image, platform, server.server_port, root)
    if not untrusted.returncode or len(server.requests) != 1:
        raise RuntimeError(f"untrusted certificate was accepted: {untrusted.stderr}")
    if not any(cause in untrusted.stderr.lower() for cause in ("unknownissuer", "unknown issuer", "self-signed certificate")):
        raise RuntimeError(f"expected certificate-specific trust failure: {untrusted.stderr}")
    mismatch = run_cli(image, platform, server.server_port, root, certificate, "wrong.fixture")
    if not mismatch.returncode or len(server.requests) != 1:
        raise RuntimeError(f"wrong certificate hostname was accepted: {mismatch.stderr}")
    if not any(cause in mismatch.stderr.lower() for cause in ("notvalidforname", "not valid for name", "hostname mismatch")):
        raise RuntimeError(f"expected certificate-specific hostname failure: {mismatch.stderr}")


def check_image(image, platform, root):
    """Serve the temporary TLS fixture while testing the container API client."""
    workflows = root / "repo" / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "fixture.yml").write_text("jobs:\n  fixture:\n    steps:\n      - uses: fixture/action@v1\n")
    certificate, key = create_certificate(root)
    # Bridge-network containers need a reachable host interface. This ephemeral
    # TLS fixture serves only a fabricated SHA, with no secrets or mutable API.
    server = ThreadingHTTPServer(("0.0.0.0", 0), Handler)  # nosec B104
    server.requests = []
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(root / "leaf.crt", key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        check_tls(image, platform, server, certificate, root)
        print(f"GitHub Actions container HTTPS verified: {image}; trusted request, untrusted/hostname rejection")
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def image_reference(value):
    """Accept image names, tags and digests, never CLI options or shell syntax."""
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._/:@-]*", value):
        raise argparse.ArgumentTypeError("expected a Docker image name, tag or digest")
    return value


def main():
    """Parse image options and run the temporary HTTPS fixture."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", type=image_reference)
    parser.add_argument("--platform", choices=("linux/amd64", "linux/arm64"))
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="github-actions-https-") as directory:
        check_image(args.image, args.platform, Path(directory))


if __name__ == "__main__":
    main()
