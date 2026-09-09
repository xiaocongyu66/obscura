FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
        perl \
        make \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency compilation by copying manifests first
COPY Cargo.toml Cargo.lock ./
COPY crates/obscura-net/Cargo.toml       crates/obscura-net/Cargo.toml
COPY crates/obscura-mcp/Cargo.toml       crates/obscura-mcp/Cargo.toml
COPY crates/obscura-cli/Cargo.toml       crates/obscura-cli/Cargo.toml

# Create stub src files so cargo can resolve the dependency graph
RUN for crate in obscura-net obscura-mcp obscura-embedder; do \
        mkdir -p crates/$crate/src && echo "// stub" > crates/$crate/src/lib.rs; \
    done && \
    mkdir -p crates/obscura-cli/src && \
    echo "fn main() {}" > crates/obscura-cli/src/main.rs && \
    echo "mod original_fetch;" > crates/obscura-cli/src/original_fetch.rs

RUN cargo build --release --bin obscura 2>/dev/null || true

ARG OBSCURA_VERSION

# Copy real sources and build
COPY crates/ crates/
RUN echo "Building Obscura version ${OBSCURA_VERSION:-from Cargo.toml}" && \
    touch crates/*/src/*.rs && cargo build --release --bin obscura

# ---

# distroless/cc: glibc + libgcc + CA certs only — no shell, no package manager
FROM gcr.io/distroless/cc-debian12

COPY --from=builder /build/target/release/obscura /obscura

EXPOSE 9222

# Bind to 0.0.0.0 so the port is reachable via `docker run -p 9222:9222`.
# Native binary still defaults to 127.0.0.1 (loopback only) — this override
# is just for the container.
ENTRYPOINT ["/obscura"]
CMD ["serve", "--port", "9222", "--host", "0.0.0.0"]
