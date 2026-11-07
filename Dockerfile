# CI builds the binaries natively per arch (see .github/workflows); this only
# copies them in, so no QEMU is needed. distroless/cc has glibc + CA certs
# (rustls verifies against the system store) and runs as uid 65532.
FROM gcr.io/distroless/cc-debian13:nonroot
ARG TARGETARCH
COPY docker-bin/bitwarden-secrets-operator-${TARGETARCH} /app/bitwarden-secrets-operator
ENTRYPOINT ["/app/bitwarden-secrets-operator"]
