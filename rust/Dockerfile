# The server image. Build it from the REPO ROOT, which is where rust-toolchain.toml lives and rust/ does not see it:
#   docker build -f rust/Dockerfile -t memory-industry-server:$(date +%Y%m%d) .
# The compiler is the one rust-toolchain.toml pins (CI-K1), read here and not written here. The base image only brings rustup
# and Debian; its own compiler is never used. Bases are pinned by digest (2026-11-11): `rust:1-slim-trixie`,
# `debian:trixie-slim`. Both are Debian 13, so the binary links the glibc it runs on. Moving a digest is a commit.
# No test runs in the build: the local judge runs them, and a build that swallows a red suite (`|| true`) says nothing.

FROM rust:1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7 AS builder

# Caps cargo's parallelism for the one build that runs outside the judge's cgroup (docker build). Empty: every CPU.
ARG JOBS

# Build scripts compile C and C++ (mimalloc, zstd, tree-sitter).
RUN apt-get update && \
    apt-get install -y --no-install-recommends g++ && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY rust-toolchain.toml ./
RUN channel="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' rust-toolchain.toml)" && \
    [ -n "$channel" ] && \
    rustup toolchain install "$channel" --profile minimal

WORKDIR /build/rust
COPY rust/Cargo.toml rust/Cargo.lock ./
COPY rust/src src
COPY rust/migrations migrations
COPY rust/embed embed
RUN cargo build --release --locked --bin memory-industry ${JOBS:+--jobs "$JOBS"}

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

RUN groupadd --system app && \
    useradd --system --create-home --gid app app

COPY --from=builder /build/rust/target/release/memory-industry /usr/local/bin/memory-industry

# The database is never created from here: there is no Docker inside. Without DATABASE_URL the server refuses and says so.
# ONNX Runtime is not in the image (`ort` loads it at run time): mount it and set ORT_DYLIB_PATH, with ONNX_MODEL_PATH.
ENV MEMORY_INDUSTRY_REQUIRE_DATABASE_URL=1

STOPSIGNAL SIGTERM
USER app
ENTRYPOINT ["memory-industry"]
