FROM rust:1.68.0-bullseye as builder
WORKDIR /app
# https://users.rust-lang.org/t/cargo-uses-too-much-memory-being-run-in-qemu/76531
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
RUN cargo init
COPY Cargo.toml Cargo.lock /app/
RUN cargo build --release
COPY src/ /app/src/
RUN find src/ -type f -exec touch {} + && cargo build --release

FROM node:19.7.0-bullseye-slim
WORKDIR /app
RUN npm install -g @bitwarden/cli
COPY --from=builder /app/target/release/bitwarden-secrets-operator /app/
ENTRYPOINT [ "/app/bitwarden-secrets-operator" ]
