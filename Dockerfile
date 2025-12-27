FROM busybox:1.37.0 AS rename
WORKDIR /app
COPY docker-bin/bitwarden-secrets-operator-arm64 bitwarden-secrets-operator-arm64
COPY docker-bin/bitwarden-secrets-operator-amd64 bitwarden-secrets-operator-amd64

# lts node
FROM node:24.12.0-bookworm-slim
WORKDIR /app
ARG TARGETARCH
RUN npm install -g @bitwarden/cli@2025.4.0
COPY --from=rename /app/bitwarden-secrets-operator-$TARGETARCH /app/bitwarden-secrets-operator
ENTRYPOINT [ "/app/bitwarden-secrets-operator" ]
