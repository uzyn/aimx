# syntax=docker/dockerfile:1.6

# ---------- Builder ----------
# Pinned to a specific minor tag so upstream re-tags of `rust:1-bookworm` do
# not silently change the compiler used for builds. Bump this deliberately.
FROM rust:1.95-bookworm AS builder

WORKDIR /build

# Prime the dependency cache with just the manifests and a stub binary. This
# layer only needs to change when Cargo.toml / Cargo.lock change, so subsequent
# rebuilds that only touch src/ reuse the cached dependency compile.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() { println!("stub"); }' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/deps/aimx_verifier* target/release/aimx-verifier*

# Now copy the real sources and build for real. The load-bearing step here is
# `touch src/main.rs`: it guarantees Cargo invalidates the leaf-crate
# fingerprint even if COPY preserves mtimes from the host build context. The
# stub-build `rm -rf` in the previous layer is just defense-in-depth to
# discard the stub artifacts — the touch is what actually forces a rebuild.
COPY src ./src
RUN touch src/main.rs \
    && cargo build --release --locked \
    && strip target/release/aimx-verifier

# ---------- Runtime ----------
FROM debian:bookworm-slim AS runtime

# `curl` is installed alongside `ca-certificates` so the HEALTHCHECK below can
# hit /health from inside the container without dragging in a second tool.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/aimx-verifier /usr/local/bin/aimx-verifier

# Port 25  -> built-in SMTP listener (outbound reachability target)
# Port 3025 -> loopback-default HTTP listener (behind Caddy on the host)
EXPOSE 25 3025

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3025/health || exit 1

# Runs as root by design so the process can bind port 25 without capability
# fiddling. The service is a short SMTP responder and two HTTP endpoints; it
# does not touch the filesystem.
ENTRYPOINT ["/usr/local/bin/aimx-verifier"]
