# syntax=docker/dockerfile:1

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
# `1-slim` rather than a pinned patch: Cargo.lock pins every dependency, and
# the crate needs Rust >= 1.87 (`u64::is_multiple_of`). The slim image ships the
# C toolchain that `ring` needs; `gcc`/`libc6-dev` are installed explicitly so a
# future base-image change cannot silently remove the linker.
FROM rust:1-slim-bookworm AS build

RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# The whole workspace in one copy, then one release build. A dependency-cache
# layer would save rebuild time, but it needs a dummy source tree for every
# target and breaks quietly when the workspace layout changes.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# `[profile.release]` is LTO + codegen-units=1, so this takes several minutes.
RUN cargo build --release --locked --bin fqapi-server

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim

# curl exists only for HEALTHCHECK. The binary itself needs no CA bundle:
# reqwest is built against webpki-roots, which compiles the roots in, so there
# is no openssl and no ca-certificates here.
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home fqapi \
 && mkdir -p /data \
 && chown fqapi:fqapi /data

COPY --from=build /src/target/release/fqapi-server /usr/local/bin/fqapi-server

# GPL-3.0 section 6: shipping the image is distribution, so the licence and the
# upstream attribution travel with it.
COPY LICENSE NOTICE /usr/share/doc/fqapi-server/

USER fqapi
WORKDIR /data
VOLUME ["/data"]
EXPOSE 18921

# /healthz, not /readyz: readiness waits on upstream device registration, and
# restarting a container because its pool is still warming up would be wrong.
HEALTHCHECK --interval=30s --timeout=3s --start-period=40s --retries=3 \
  CMD curl -fsS http://127.0.0.1:18921/healthz || exit 1

ENTRYPOINT ["fqapi-server"]
# The token has no default on purpose: the server refuses to start without one.
CMD ["--data-dir", "/data", "--listen", "0.0.0.0:18921"]
