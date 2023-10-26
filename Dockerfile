FROM alpine:edge as rename
WORKDIR /app
COPY target/aarch64-unknown-linux-gnu/release/bitwarden-secrets-operator bitwarden-secrets-operator-arm64
COPY target/x86_64-unknown-linux-gnu/release/bitwarden-secrets-operator bitwarden-secrets-operator-amd64

FROM alpine:edge
WORKDIR /app
ARG TARGETARCH
COPY repositories /etc/apk/repositories
RUN apk add rbw@testing gcompat
# for some reason I can't compile bso for musl in github-actions
COPY --from=rename /app/bitwarden-secrets-operator-$TARGETARCH /app/bitwarden-secrets-operator
ENTRYPOINT [ "/app/bitwarden-secrets-operator" ]
