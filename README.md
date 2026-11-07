# bitwarden-secrets-operator

A Rust Kubernetes operator that turns `BitwardenSecret` resources into
Kubernetes `Secret`s, with
[Bitwarden Secrets Manager](https://bitwarden.com/products/secrets-manager/)
as the source of truth. It talks to Secrets Manager through Bitwarden's
official Rust SDK. It's a hobby project for a single homelabber per cluster,
not for multi-tenant clusters: any namespace can request any secret the
operator can see.

## Security model

The operator authenticates with a **machine account access token**. A machine
account only sees the projects you grant it, so give it **read** access to one
project that holds the cluster's secrets. The token can't reach your Password
Manager vault or any other project, and you can revoke it on its own. One
machine account per cluster lets you revoke clusters independently.

Hardening in `examples/operator.yaml`:

- **Write-only RBAC on Secrets** (`create`, `patch`). The operator can't list
  or read the other Secrets in the cluster. Owner references clean up a Secret
  when its `BitwardenSecret` is deleted.
- **Distroless, non-root, read-only root filesystem, no capabilities,
  RuntimeDefault seccomp.** The image is just the binary and CA certificates.
- **No local state.** Secrets are held in memory only.

Also worth doing yourself:

- Lock down egress with a NetworkPolicy so the pod can reach only DNS, the
  Kubernetes API and Bitwarden (FQDN rules need Cilium, Istio or similar).
- Make sure nothing else in the cluster can read Secrets cluster-wide, and
  enable encryption at rest for Secrets in etcd.
- Pin the image to a `sha-<commit>` tag instead of `latest`.

## Install

1. In the Bitwarden web vault, open Secrets Manager for your organization (the
   free plan covers 3 projects and 3 machine accounts). Create a project for
   the cluster, then a machine account with **Can read** on that project, and
   generate an access token.
2. Store the token (hidden prompt; it goes in via stdin, so not into shell
   history or `ps`). Re-running it rotates the token in place:

   ```sh
   kubectl create namespace bitwarden-secrets-operator
   read -rsp 'token: ' t && printf %s "$t" | kubectl create secret generic bitwarden-credentials \
     -n bitwarden-secrets-operator --from-file=BWS_ACCESS_TOKEN=/dev/stdin \
     --dry-run=client -o yaml | kubectl apply -f -; unset t
   ```

   After rotating, `kubectl -n bitwarden-secrets-operator rollout restart
   deployment bitwarden-secrets-operator`.
3. Apply the CRD and the operator:

   ```sh
   kubectl apply -f examples/crd.yaml
   kubectl apply -f examples/operator.yaml # pin the image first
   ```

## Usage

```yaml
apiVersion: jrcichra.dev/v1
kind: BitwardenSecret
metadata:
  name: google-credentials # name of the resulting Secret (same namespace)
spec:
  name: google # Secrets Manager secret name
  key: notes # optional: data key for a plain value (default `notes`)
  type: Opaque # optional: Secret type, e.g. kubernetes.io/dockerconfigjson
```

| Secrets Manager value                          | Secret keys                         |
| ---------------------------------------------- | ----------------------------------- |
| exactly `{"username": "…", "password": "…"}`   | `username`, `password`              |
| anything else                                  | `notes`, or whatever `spec.key` says |

More examples are in `examples/object.yaml`. Changes in Secrets Manager reach
the `Secret` within `RECONCILE_INTERVAL` seconds.

## Configuration

| Env / flag           | Default                      | Meaning                                              |
| -------------------- | ---------------------------- | ---------------------------------------------------- |
| `BWS_ACCESS_TOKEN`   | (required)                   | Machine account access token                         |
| `NAMESPACE`          | (required)                   | Namespace of the leader-election lease (downward API) |
| `LEASE_NAME`         | `bitwarden-secrets-operator` | Name of the leader-election `Lease`                  |
| `RECONCILE_INTERVAL` | `300`                        | Seconds between refreshes and `Secret` re-applies    |
| `METRICS_PORT`       | `8000`                       | Prometheus `/metrics` port (served by the leader)    |

Metrics: `last_reconcile`, `reconcile_success_total`,
`reconcile_failure_total`, `cached_secrets`, `cache_refresh_success_total`,
`cache_refresh_failure_total`.

## Development

```sh
cargo clippy --all-targets && cargo test
cargo run -- --generate-crd > examples/crd.yaml # after changing the CRD
```

CI builds native amd64 and arm64 binaries and publishes
`ghcr.io/jrcichra/bitwarden-secrets-operator` with `latest` and
`sha-<8-char commit>` tags.

## Disclaimer

This is a hobby project and I can't guarantee the safety of your secrets. Use
it at your own risk.
