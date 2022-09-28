FROM rust:1.64.0-bullseye as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM node:18.9.1-bullseye
WORKDIR /app
RUN npm install -g @bitwarden/cli
COPY --from=builder /app/target/release/bitwarden-secrets-operator /app/
ENTRYPOINT [ "/app/bitwarden-secrets-operator" ]
