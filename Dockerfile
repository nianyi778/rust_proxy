# syntax=docker/dockerfile:1

FROM rust:1.75-slim AS builder
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-musl

COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN cargo build --release --target x86_64-unknown-linux-musl

FROM alpine:3.19
RUN apk add --no-cache ca-certificates

ENV LISTEN_ADDR=0.0.0.0:8080
EXPOSE 8080

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/rust_proxy /usr/local/bin/rust_proxy

ENTRYPOINT ["/usr/local/bin/rust_proxy"]
