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

- **Narrow RBAC.** The operator needs `create`/`patch`/`list`/`watch` on
  Secrets (to apply them and to restore one that's edited or deleted) and
  `patch` on `bitwardensecrets/status`, plus `create`/`patch` on Events. It only watches Secrets carrying its
  `app.kubernetes.io/managed-by` label, but RBAC can't express that, so
  treat the service account as able to read Secrets cluster-wide. Owner
  references clean up a Secret when its `BitwardenSecret` is deleted.
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
the `Secret` within `RECONCILE_INTERVAL` seconds (each poll that sees a change
reconciles everything at once). A `Secret` that's edited or deleted by hand is
put back immediately, and changing `spec.type` recreates the `Secret` (its type
is immutable).

Each `BitwardenSecret` reports a `Ready` condition:

```console
$ kubectl get bws -A
NAMESPACE   NAME          SECRET        READY   REASON     AGE
shed        shed-ark-db   shed-ark-db   True    Synced     2y
searxng     api-key       api-key       False   NotFound   3d
$ kubectl wait bws/shed-ark-db --for=condition=Ready
```

Each change of that condition is also recorded as an Event, so
`kubectl describe bws <name>` shows when it broke or recovered and why.

## Configuration

| Env / flag           | Default                      | Meaning                                              |
| -------------------- | ---------------------------- | ---------------------------------------------------- |
| `BWS_ACCESS_TOKEN`   | (required)                   | Machine account access token                         |
| `NAMESPACE`          | (required)                   | Namespace of the leader-election lease (downward API) |
| `POD_NAME`           | (required)                   | Lease holder identity (downward API)                 |
| `LEASE_NAME`         | `bitwarden-secrets-operator` | Name of the leader-election `Lease`                  |
| `RECONCILE_INTERVAL` | `300`                        | Seconds between Secrets Manager polls                |
| `METRICS_PORT`       | `8000`                       | Prometheus `/metrics` port (served by the leader)    |

Metrics: `last_reconcile` (Unix time of the last successful reconcile),
`cached_secrets`, `reconcile_failure_total{reason}` (the `Ready` reason),
`cache_refresh_failure_total`. For per-object readiness, have
kube-state-metrics export the `Ready` condition (`examples/kube-state-metrics.yaml`).

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
