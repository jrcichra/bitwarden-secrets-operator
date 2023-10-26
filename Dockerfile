FROM alpine as rename
WORKDIR /app
COPY target/aarch64-unknown-linux-gnu/release/bitwarden-secrets-operator bitwarden-secrets-operator-arm64
COPY target/x86_64-unknown-linux-gnu/release/bitwarden-secrets-operator bitwarden-secrets-operator-amd64

FROM gcr.io/distroless/base-debian12:nonroot
WORKDIR /app
ARG TARGETARCH
COPY /home/runner/cargo/bin/rbw /usr/local/bin/rbw
COPY --from=rename /app/bitwarden-secrets-operator-$TARGETARCH /app/bitwarden-secrets-operator
ENTRYPOINT [ "/app/bitwarden-secrets-operator" ]
