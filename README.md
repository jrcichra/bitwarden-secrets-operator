# bitwarden-secrets-operator

A Rust Kubernetes operator that reconciles `BitwardenSecrets` into `Secrets`.

In other words, you can reference Bitwarden passwords from Kubernetes natively through `Secrets`.

This is currently designed for a single customer/homelabber per cluster model, not a multi-tenant model, as there's no restriction on `BitwardenSecret` per namespace. It's made to be a step up for people who manage their secrets in Bitwarden and want to reference them in their personal Kubernetes cluster using GitOps.

## Secret scope

By default, the operator will only reconcile secrets found in the `kubernetes` directory of your Bitwarden store. Limiting it to a folder reduces the likelyhood a `Secret` will be made with unintentional credentials. Unfortunately, Bitwarden falls short in API accounts, and all accounts have full access to all features. Ideally this operator would be limited to reading secrets in the `kubernetes` directory. Maybe this is possible with organizations? I'm not sure.

# Getting Started

To get started, audit and run `login.sh`, providing your Bitwarden API key as [described here](https://bitwarden.com/help/personal-api-key/#authenticate-using-your-api-key), and providing your password as required by [unlock](https://bitwarden.com/help/cli/#unlock-options).
The script will create a `Secret` called `bitwarden-credentials`. The values within this secret will be referenced by `bitwarden-secrets-operator` to maintain a session with Bitwarden and require no human interaction.

To protect your vault from a potential hacker's malicious build of `bitwarden-secrets-operator`, apply a `NetworkPolicy` so `bitwarden-secrets-operator` can only talk to the necessary Bitwarden API endpoints with something like an [Istio EgressRule](https://istio.io/v0.2/docs/reference/config/traffic-rules/egress-rules.html) if you're using Istio and the Kubernetes API. Also make sure other applications do not have a `ClusterRoleBinding` to view secrets and make sure your Kubernetes API is secured.

# Usage

See `examples/object.yaml` for examples.

If you make a `login` secret in Bitwarden, this translates to a `Secret` with `username` and `password` keys.

If you make a `secure note` secret in Bitwarden, this translates to a `Secret` with a `notes` key, as it is described in their CLI output.

There are optional keys for `key` and `type`, which correlate to the fields on the `Secret`.

# Disclaimer

This is a hobby project. I cannot guarantee the safety of your passwords with this solution. Use at your own risk.

I'm using it in my homelab cluster to manage my secrets. It's currently satisfying my simple use cases.

I know Rust enough to write code that compiles, but not much more than that. The code style will be rough.
