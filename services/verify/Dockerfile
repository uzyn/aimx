# syntax=docker/dockerfile:1.6

# ---------- Builder ----------
FROM rust:1-bookworm AS builder

WORKDIR /build

# Prime the dependency cache with just the manifests and a stub binary. This
# layer only needs to change when Cargo.toml / Cargo.lock change, so subsequent
# rebuilds that only touch src/ reuse the cached dependency compile.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() { println!("stub"); }' > src/main.rs \
    && cargo build --release \
    && rm -rf src target/release/deps/aimx_verify* target/release/aimx-verify*

# Now copy the real sources and build for real. `touch` guarantees Cargo
# invalidates the leaf-crate fingerprint even if COPY preserves mtimes from
# the host build context.
COPY src ./src
RUN touch src/main.rs \
    && cargo build --release \
    && strip target/release/aimx-verify

# ---------- Runtime ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/aimx-verify /usr/local/bin/aimx-verify

# Port 25  -> built-in SMTP listener (outbound reachability target)
# Port 3025 -> loopback-default HTTP listener (behind Caddy on the host)
EXPOSE 25 3025

# Runs as root by design so the process can bind port 25 without capability
# fiddling. The service is a short SMTP responder and two HTTP endpoints; it
# does not touch the filesystem.
ENTRYPOINT ["/usr/local/bin/aimx-verify"]
