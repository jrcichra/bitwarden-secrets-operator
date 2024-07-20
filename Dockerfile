FROM alpine as rename
WORKDIR /app
COPY target/aarch64-unknown-linux-gnu/release/bitwarden-secrets-operator bitwarden-secrets-operator-arm64
COPY target/x86_64-unknown-linux-gnu/release/bitwarden-secrets-operator bitwarden-secrets-operator-amd64

FROM oven/bun:1.1.20
WORKDIR /app
ARG TARGETARCH
RUN bun install -g @bitwarden/cli
COPY --from=rename /app/bitwarden-secrets-operator-$TARGETARCH /app/bitwarden-secrets-operator
ENTRYPOINT [ "/app/bitwarden-secrets-operator" ]
