# ThreatFlux Rust Dockerfile
# Multi-stage build for single-crate or workspace-based applications.
#
# Debian 13 (trixie) throughout: the builder links the binary against trixie's
# glibc, and the runtime is distroless (glibc, libgcc, CA certificates, no
# shell or package manager). The maintainer binary talks to the GitHub and
# crates.io APIs over rustls, so it needs nothing else. (Only a local
# `update --cargo` that rewrites files runs `cargo update`, which no published
# image has ever shipped.)
#
# Base images are pinned by multi-arch index digest. Dependabot refreshes the
# Rust builder; refresh the runtime with
#   docker buildx imagetools inspect gcr.io/distroless/cc-debian13:nonroot

FROM rust:1.99.0-trixie@sha256:6ff07edce8775d0f64be7aba9197229407301bddf2054d62c27b541a6238a181 AS rust-base

ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}

ARG VERSION=0.0.0
ARG BUILD_DATE=unknown
ARG VCS_REF=unknown
ARG BINARY_NAME=github-actions-maintainer
ARG BINARY_PACKAGE=
ARG SBOM_MANIFEST_PATH=Cargo.toml
ARG OCI_IMAGE_TITLE=Rust Application
ARG OCI_IMAGE_DESCRIPTION=Rust Application
ARG OCI_IMAGE_VENDOR=
ARG OCI_IMAGE_SOURCE=https://github.com

# pkg-config and libssl-dev build cargo-cyclonedx; tini is installed only so
# the runtime stage can copy it, because distroless ships no init and PID 1
# has to forward signals and reap children. Package revisions follow the
# digest-pinned base image.
# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    pkg-config \
    libssl-dev \
    tini \
    && rm -rf /var/lib/apt/lists/*

FROM rust-base AS builder

RUN if ! id -u builder >/dev/null 2>&1; then \
      groupadd -f builder && useradd -m -o -u 1000 -g builder builder; \
    fi
USER builder
WORKDIR /build

COPY --chown=builder:builder . .

RUN rustc --version --verbose && cargo --version && \
    if [ -n "${BINARY_PACKAGE}" ]; then \
      cargo build --locked --release -p "${BINARY_PACKAGE}" --bin "${BINARY_NAME}" --all-features; \
    else \
      cargo build --locked --release --bin "${BINARY_NAME}" --all-features; \
    fi

RUN cargo install cargo-cyclonedx --locked --version 0.5.9 && \
    cargo cyclonedx \
      --manifest-path "${SBOM_MANIFEST_PATH}" \
      --all-features \
      --format json \
      --spec-version 1.5 \
      --override-filename "${BINARY_NAME}-sbom"

# nonroot runs as uid/gid 65532 with HOME=/home/nonroot. The GitHub Action
# image (runtime/Dockerfile) switches back to root, as Docker actions need.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2 AS runtime

ARG VERSION=0.0.0
ARG BUILD_DATE=unknown
ARG VCS_REF=unknown
ARG BINARY_NAME=github-actions-maintainer
ARG OCI_IMAGE_TITLE=Rust Application
ARG OCI_IMAGE_DESCRIPTION=Rust Application
ARG OCI_IMAGE_VENDOR=
ARG OCI_IMAGE_SOURCE=https://github.com

LABEL org.opencontainers.image.title="${OCI_IMAGE_TITLE}" \
      org.opencontainers.image.description="${OCI_IMAGE_DESCRIPTION}" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.vendor="${OCI_IMAGE_VENDOR}" \
      org.opencontainers.image.source="${OCI_IMAGE_SOURCE}"

# There is no shell to run mkdir or chown in, so everything arrives by COPY.
# The binary and the SBOM stay owned by root: the runtime user can run and
# read them but not replace them.
COPY --from=rust-base /usr/bin/tini /usr/bin/tini
COPY --from=builder /build/target/release/${BINARY_NAME} /usr/local/bin/app
COPY --from=builder /build/${BINARY_NAME}-sbom.json /usr/share/doc/app/sbom.cdx.json

USER 65532:65532
WORKDIR /home/nonroot

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/app"]
CMD []
