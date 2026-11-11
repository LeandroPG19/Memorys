# The PyPI route, from this tree: the wheel `pip install memory-industry` would fetch, built here and installed into the
# Python image people run it on. Build it from the REPO ROOT:
#   docker build -t memory-industry-wheel:$(date +%Y%m%d) .
# pyproject.toml makes pip run maturin, which runs cargo, so the builder is a Rust image; the compiler is the one
# rust-toolchain.toml pins (CI-K1), read here and not written here. Bases are pinned by digest (2026-11-11):
# `rust:1-slim-trixie`, `python:3.14-slim`. Moving a digest is a commit.

FROM rust:1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7 AS wheel

# Caps cargo's parallelism for the one build that runs outside the judge's cgroup (docker build). Empty: every CPU.
ARG JOBS

# Build scripts compile C and C++ (mimalloc, zstd, tree-sitter); pip needs a venv on Debian.
RUN apt-get update && \
    apt-get install -y --no-install-recommends g++ python3-venv && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY rust-toolchain.toml ./
RUN channel="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' rust-toolchain.toml)" && \
    [ -n "$channel" ] && \
    rustup toolchain install "$channel" --profile minimal

COPY pyproject.toml README.md ./
COPY rust/Cargo.toml rust/Cargo.lock rust/
COPY rust/src rust/src
COPY rust/migrations rust/migrations
COPY rust/embed rust/embed
RUN python3 -m venv /venv && \
    env ${JOBS:+CARGO_BUILD_JOBS=$JOBS} /venv/bin/pip wheel --no-deps --wheel-dir /wheels .

FROM python:3.14-slim@sha256:9aaa474097d094040649f2162a627c0049b408f52138d980916788d80f04802f

RUN --mount=type=bind,from=wheel,source=/wheels,target=/wheels \
    pip install --no-cache-dir --no-index /wheels/*.whl

RUN groupadd --system app && \
    useradd --system --create-home --gid app app

# The database is never created from here: there is no Docker inside. Without DATABASE_URL the server refuses and says so.
ENV MEMORY_INDUSTRY_REQUIRE_DATABASE_URL=1

STOPSIGNAL SIGTERM
USER app
ENTRYPOINT ["memory-industry"]
