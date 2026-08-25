# Orbit-Transfer edge relay — minimal container image.
#
#   docker build -t orbittransfer/relay .
#   docker run --rm -p 9000:9000 orbittransfer/relay
#
# The relay is transport-agnostic (routes sealed binary symbols over
# WebSocket by session id); `--throttle-kbps` optionally simulates a
# constrained uplink per connection.
FROM rust:1.91-slim AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/

RUN cargo build --release -p orbit-relay --bin orbit-relay

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/orbit-relay /usr/local/bin/orbit-relay

EXPOSE 9000
ENTRYPOINT ["orbit-relay", "serve", "--addr", "0.0.0.0:9000"]