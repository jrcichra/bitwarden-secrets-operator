# bitwarden-secrets-operator

A Rust Kubernetes operator that turns `BitwardenSecret` resources into
Kubernetes `Secret`s, using Bitwarden Password Manager items as the source of
truth. It's a hobby project for a single homelabber per cluster, not for
multi-tenant clusters: any namespace can request any item the operator can
see.

## Security model: read this first

The operator logs in with the Bitwarden CLI (`bw`), so it needs a **personal
API key plus the master password**. Together they unlock the *entire* vault
of that account. The folder filter (below) limits what gets synced; it doesn't
limit what someone holding those credentials can read.

Treat the `bitwarden-credentials` Secret as being as sensitive as your vault
itself. If that's more exposure than you want, use a dedicated account for the
cluster: share only the cluster items with it, read-only, through a free
organization, and put them in that account's `kubernetes` folder. A leak then
exposes only those items.

If you want scoped, read-only, no-master-password access, look at
[Bitwarden Secrets Manager](https://bitwarden.com/help/secrets-manager-kubernetes-operator/)
or [External Secrets Operator](https://external-secrets.io/latest/provider/bitwarden-secrets-manager/)
instead.

Hardening that's already in `examples/operator.yaml`:

- **Write-only RBAC on Secrets** (`create`, `patch`). The operator can't list
  or read the other Secrets in the cluster. Owner references clean up Secrets
  when their `BitwardenSecret` is deleted.
- **Non-root, read-only root filesystem, no capabilities, RuntimeDefault
  seccomp.**
- **RAM-backed `bw` state.** The CLI's cache (`data.json`, the encrypted
  vault) lives in a `medium: Memory` emptyDir, never on node disk.
- **Stable device ID** (`BW_DEVICE_ID`). Without it, every pod start is a new
  CLI install with a random device ID, and Bitwarden emails a "new device
  login" each time. A wrapper seeds `global_applicationId_appId` into
  `data.json` before the first login. That key is internal `bw` state, so
  check that it still matches after a CLI upgrade. Use one ID per cluster
  (`uuidgen`).

Also worth doing yourself:

- Lock down egress with a NetworkPolicy so the pod can reach only DNS, the
  Kubernetes API and Bitwarden (FQDN rules need Cilium, Istio or similar).
- Make sure nothing else in the cluster can read Secrets cluster-wide, and
  enable encryption at rest for Secrets in etcd.
- Pin the image to a `sha-<commit>` tag instead of `latest`.

## Install

1. Generate a [personal API key](https://bitwarden.com/help/personal-api-key/)
   and put the items you want synced in a `kubernetes` folder.
2. Run `./login.sh` (read it first). It prompts for the client ID, client
   secret and password, then creates the `bitwarden-credentials` Secret in the
   `bitwarden-secrets-operator` namespace.
3. Apply the CRD and the operator:

   ```sh
   kubectl apply -f examples/crd.yaml
   # set BW_DEVICE_ID and pin the image first
   kubectl apply -f examples/operator.yaml
   ```

## Usage

```yaml
apiVersion: jrcichra.dev/v1
kind: BitwardenSecret
metadata:
  name: google-credentials # name of the resulting Secret (same namespace)
spec:
  name: google # Bitwarden item name, inside the folder
  key: notes # optional: key name for a secure note (default `notes`)
  type: Opaque # optional: Secret type, e.g. kubernetes.io/dockerconfigjson
```

| Bitwarden item | Secret keys                              |
| -------------- | ---------------------------------------- |
| Login          | `username`, `password`                   |
| Secure note    | `notes`, or whatever `spec.key` says     |

More examples are in `examples/object.yaml`.

## Configuration

| Env / flag           | Default      | Meaning                                         |
| -------------------- | ------------ | ----------------------------------------------- |
| `FOLDER`             | `kubernetes` | Bitwarden folder to sync items from             |
| `RECONCILE_INTERVAL` | `300`        | Seconds between vault refreshes and re-applies  |
| `METRICS_PORT`       | `8000`       | Prometheus `/metrics` port                      |
| `NAMESPACE`          | (required)   | Namespace for the leader lease (downward API)   |
| `BW_CLIENTID`, `BW_CLIENTSECRET`, `BW_PASSWORD` | (required) | Bitwarden login |

Metrics: `last_reconcile`, `reconcile_success_total`,
`reconcile_failure_total`, `cached_secrets`, `cache_refresh_success_total`,
`cache_refresh_failure_total`.

## Disclaimer

This is a hobby project and I can't guarantee the safety of your passwords.
Use it at your own risk.
