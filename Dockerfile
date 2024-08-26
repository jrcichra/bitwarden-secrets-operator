FROM busybox:1.36.1 as rename
WORKDIR /app
COPY target/aarch64-unknown-linux-gnu/release/bitwarden-secrets-operator bitwarden-secrets-operator-arm64
COPY target/x86_64-unknown-linux-gnu/release/bitwarden-secrets-operator bitwarden-secrets-operator-amd64

# lts node
FROM node:21.7.3-bookworm-slim
WORKDIR /app
ARG TARGETARCH
RUN npm install -g @bitwarden/cli
COPY --from=rename /app/bitwarden-secrets-operator-$TARGETARCH /app/bitwarden-secrets-operator
ENTRYPOINT [ "/app/bitwarden-secrets-operator" ]
