# One image for every part of the review box: the dispatcher, the egress proxy, and the
# job containers (see job.mjs). Build context: the plugin directory.
FROM docker:29-cli AS docker-cli

FROM node:24-slim
ARG CODEX_VERSION=0.160.1
# Matches rust-toolchain.toml on the track branches; bump both together. Fix jobs build and
# test with it in their runner container (offline: dependencies are fetched first by the deps
# step); the Codex containers get it hidden.
ARG RUST_TOOLCHAIN=1.93.1
# procps: scripts/limit.sh (ps) in the bench steps.
RUN apt-get update \
 && apt-get install -y --no-install-recommends git jq ca-certificates curl gcc libc6-dev procps \
 && rm -rf /var/lib/apt/lists/*
# hyperfine for the bench steps: the prebuilt release, pinned like CI's, checked against its digest.
ARG HYPERFINE_VERSION=1.20.0
ARG HYPERFINE_SHA256=3285ec7959285288137043dd81dce0dde056227018a8277532d9a364b4f03c2b
RUN curl --proto '=https' --tlsv1.2 -sSfL -o /tmp/hyperfine.tgz \
      "https://github.com/sharkdp/hyperfine/releases/download/v${HYPERFINE_VERSION}/hyperfine-v${HYPERFINE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
 && echo "${HYPERFINE_SHA256}  /tmp/hyperfine.tgz" | sha256sum -c - \
 && tar -xzf /tmp/hyperfine.tgz -C /usr/local/bin --strip-components=1 "hyperfine-v${HYPERFINE_VERSION}-x86_64-unknown-linux-musl/hyperfine" \
 && rm /tmp/hyperfine.tgz && hyperfine --version
ENV RUSTUP_HOME=/usr/local/rustup PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | CARGO_HOME=/usr/local/cargo sh -s -- -y --no-modify-path --profile minimal \
      --default-toolchain "${RUST_TOOLCHAIN}" --component rustfmt,clippy \
 && rm -rf /usr/local/cargo/registry /usr/local/rustup/downloads /usr/local/rustup/tmp \
 && rustc --version && cargo clippy --version && cargo fmt --version
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
RUN npm install -g "@openai/codex@${CODEX_VERSION}" && npm cache clean --force
# pnpm and Yarn shims for the bench repos' installs (the root file system is read-only at run time).
RUN corepack enable
WORKDIR /app
COPY box/package.json box/package-lock.json ./
RUN npm ci --omit=dev && npm cache clean --force
COPY ci/ ./ci/
COPY box/*.mjs ./
RUN printf '#!/bin/sh\nexec node /app/run-client.mjs "$@"\n' > /usr/local/bin/box-run && chmod 755 /usr/local/bin/box-run
# Fallback review instructions for before the plugin is on main (job.mjs prepare).
COPY . ./plugin/
USER node
