#!/usr/bin/env bash
# Store a Secrets Manager machine-account access token where the operator
# expects it. Uses the current kubectl context; pass e.g. `--context home` to
# pick another.
set -euo pipefail

NAMESPACE="bitwarden-secrets-operator"

read -rsp "Machine account access token: " BWS_ACCESS_TOKEN
echo

kubectl "$@" create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl "$@" apply -f -
# apply (not create) so re-running rotates the token in place
kubectl "$@" -n "$NAMESPACE" create secret generic bitwarden-credentials \
    --from-literal=BWS_ACCESS_TOKEN="$BWS_ACCESS_TOKEN" \
    --dry-run=client -o yaml | kubectl "$@" apply -f -
echo "Stored. Restart the operator to pick it up:"
echo "  kubectl $* -n $NAMESPACE rollout restart deployment bitwarden-secrets-operator"
