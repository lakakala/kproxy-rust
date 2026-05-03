# syntax=docker/dockerfile:1

ARG RUST_VERSION=1
ARG APP_NAME=kproxy-rust

FROM rust:${RUST_VERSION}-bookworm AS builder
ARG APP_NAME

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/app/target \
    cargo build --locked --release && \
    cp "/app/target/release/${APP_NAME}" /usr/local/bin/kproxy

FROM debian:bookworm-slim AS runtime

RUN groupadd --system --gid 10001 kproxy && \
    useradd \
        --system \
        --uid 10001 \
        --gid kproxy \
        --home-dir /nonexistent \
        --shell /usr/sbin/nologin \
        kproxy

WORKDIR /app

COPY --from=builder /usr/local/bin/kproxy /usr/local/bin/kproxy
COPY server.example.toml /etc/kproxy/server.toml
COPY client.example.toml /etc/kproxy/client.toml

USER kproxy:kproxy

EXPOSE 8080

ENTRYPOINT ["kproxy"]
CMD ["server", "--config", "/etc/kproxy/server.toml"]
