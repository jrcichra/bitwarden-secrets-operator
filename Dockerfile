FROM rust:1.64.0-bullseye as builder
WORKDIR /app
RUN cargo init
COPY Cargo.toml Cargo.lock /app/
RUN cargo build --release
COPY src/main.rs /app/src/main.rs
RUN touch src/main.rs && cargo build --release

FROM node:18.9.1-bullseye
WORKDIR /app
RUN npm install -g @bitwarden/cli
COPY --from=builder /app/target/release/bitwarden-secrets-operator /app/
ENTRYPOINT [ "/app/bitwarden-secrets-operator" ]
