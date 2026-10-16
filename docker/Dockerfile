# herdr-board sandbox image.
#
# Everything is pinned: the base image by digest (Rust 1.97.0 on Debian
# bookworm slim), the Herdr release asset by SHA-256 per version and
# architecture (default 0.9.0; 0.9.3 via --build-arg HERDR_VERSION=0.9.3), and
# the socket protocol by a build-time check (must be exactly 22).
#
# Build: docker build --platform linux/<amd64|arm64> [--build-arg HERDR_VERSION=<v>] -t <tag> docker/
# The sandbox wrapper (scripts/sandbox.sh) is the supported entry point.

FROM rust:1.97.0-slim-bookworm@sha256:6d220bf85c74e842a79da63997af8d2e74455c0b8847d8bb3a5888572334991d

# TARGETARCH is provided by BuildKit (amd64/arm64). The fallback covers
# classic builders: derive it from the kernel so cross-platform safety never
# depends on builder features.
ARG TARGETARCH

# Tool dependencies from the pinned Debian base. node+npm come from a pinned
# Node 22 LTS install (see below); they are only used by the explicit
# real-provider agent mode (installs the pinned provider CLIs at prepare
# time into the state volume, never into the image's config).
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    build-essential git curl ca-certificates python3 jq procps \
  && rm -rf /var/lib/apt/lists/*

# fmt/clippy components for the pinned toolchain (no-op when present).
RUN rustup component add clippy rustfmt

# Herdr — one pinned release asset per supported version and architecture,
# verified by SHA-256, exact version string, and socket protocol 22 before the
# image can be produced. An unlisted version fails the build. The 0.9.0 x86_64
# pin mirrors e2e/ci.sh.
ARG HERDR_VERSION=0.9.0
ENV HERDR_VERSION=${HERDR_VERSION}
ENV HERDR_PROTOCOL=22
RUN set -eu; \
  arch="${TARGETARCH:-$(uname -m)}"; \
  case "$arch" in \
    x86_64|amd64) asset=herdr-linux-x86_64 ;; \
    aarch64|arm64) asset=herdr-linux-aarch64 ;; \
    *) echo "unsupported architecture: $arch" >&2; exit 1 ;; \
  esac; \
  case "${HERDR_VERSION}/${asset}" in \
    0.9.0/herdr-linux-x86_64) sha=4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f ;; \
    0.9.0/herdr-linux-aarch64) sha=9c8db20fb7e7427b138d5367113f1621ffd319f2f65d6f009e2594029115f0d2 ;; \
    0.9.3/herdr-linux-x86_64) sha=18a8dc65f1c2fa485884344356dea1cfd911c6f06cf46fa78e193f4087f4dba7 ;; \
    0.9.3/herdr-linux-aarch64) sha=4de7aa3e25678812e92960de64f7c2aaa1bca1f0f80a3c5e559837e231e1f5c0 ;; \
    *) echo "no pinned SHA-256 for herdr ${HERDR_VERSION} ${asset}" >&2; exit 1 ;; \
  esac; \
  curl --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 300 --retry 3 --retry-all-errors \
    --output /usr/local/bin/herdr \
    "https://github.com/herdrdev/herdr/releases/download/v${HERDR_VERSION}/${asset}"; \
  chmod 755 /usr/local/bin/herdr; \
  printf '%s  %s\n' "$sha" /usr/local/bin/herdr | sha256sum --check; \
  [ "$(/usr/local/bin/herdr --version)" = "herdr ${HERDR_VERSION}" ]; \
  /usr/local/bin/herdr api schema --json | jq -e ".protocol == ${HERDR_PROTOCOL}" >/dev/null; \
  echo "sandbox image: herdr ${HERDR_VERSION} protocol ${HERDR_PROTOCOL} (${arch}) verified"

# Node 22 LTS — one pinned tarball per architecture, verified by SHA-256 before
# the image can be produced. pi 0.84.x requires Node >= 22.19.0 (the Debian
# bookworm node is 18, too old); the node dist tarball provides node+npm with
# its own build (npm 10.x). Provider CLIs install into the state volume at
# runtime, so node exists only to install/run them and never holds config.
ENV NODE_VERSION=22.23.2
RUN set -eu; \
  arch="${TARGETARCH:-$(uname -m)}"; \
  case "$arch" in \
    x86_64|amd64) node_asset=node-v${NODE_VERSION}-linux-x64.tar.xz; \
      node_sha=d60acfe00a2932254bb0ad20e01b0d74397a0875595de719654b214f4b03f307 ;; \
    aarch64|arm64) node_asset=node-v${NODE_VERSION}-linux-arm64.tar.xz; \
      node_sha=fff4078c5def658577f92c88db7db3bc0072924bfb93fe52c1e744a54e94abb8 ;; \
    *) echo "unsupported architecture: $arch" >&2; exit 1 ;; \
  esac; \
  curl --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 600 --retry 3 --retry-all-errors \
    --output /tmp/node.tar.xz "https://nodejs.org/dist/v${NODE_VERSION}/${node_asset}"; \
  printf '%s  %s\n' "$node_sha" /tmp/node.tar.xz | sha256sum --check; \
  tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1; \
  rm /tmp/node.tar.xz; \
  node --version | grep -qx "v${NODE_VERSION}"; \
  npm --version >/dev/null; \
  echo "sandbox image: node ${NODE_VERSION} ($arch) verified"

# Non-root runtime user. HOME and all mutable state live in named volumes
# mounted by the wrapper; the image itself stays stateless.
RUN useradd --uid 1000 --create-home --shell /bin/bash board \
  && mkdir -p /opt/cargo /opt/target /artifacts \
  && chown -R 1000:1000 /opt/cargo /opt/target /artifacts

ENV RUSTUP_HOME=/usr/local/rustup
WORKDIR /repo
USER board
