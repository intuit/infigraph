# Minimal, throwaway image for simulating index-freshness failure modes
# (issue #48): branch switch, process restart, and dirty working tree.
# Not part of CI — run manually via docker/freshness-test/run.sh.
# Kuzu (lbug) requires C++20 <format>, unavailable in Debian bookworm's
# default g++-12 (and g++-13 isn't packaged for bookworm-backports on
# arm64) — trixie ships g++-14 by default, so build there instead.
FROM rust:1-slim-trixie AS build

RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    git \
    build-essential \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
RUN cargo build --release -p infigraph-cli -p infigraph-mcp

FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    jq \
    curl \
    && rm -rf /var/lib/apt/lists/* \
    && git config --global user.email "test@example.com" \
    && git config --global user.name "Freshness Test"

# Note: `infigraph index` auto-runs SCIP enrichment (downloads a
# Python/Node toolchain for scip-python) — harmless noise here since it
# fails gracefully without a working pip, but expect it in the logs.

COPY --from=build /src/target/release/infigraph /usr/local/bin/infigraph
COPY --from=build /src/target/release/infigraph-mcp /usr/local/bin/infigraph-mcp
COPY docker/freshness-test/run.sh /usr/local/bin/run-freshness-test.sh
RUN chmod +x /usr/local/bin/run-freshness-test.sh

ENTRYPOINT ["/usr/local/bin/run-freshness-test.sh"]
