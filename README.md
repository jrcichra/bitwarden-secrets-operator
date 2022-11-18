# bitwarden-secrets-operator

A Rust Kubernetes operator that reconciles `BitwardenSecrets` into `Secrets`.

In other words, you can reference Bitwarden passwords from Kubernetes natively through `Secrets`.

This is currently designed for a single customer/homelabber per cluster model, as there's no restriction on who can make a `BitwardenSecret`. It's made to be a step up for people who manage their secrets in Bitwarden and want to reference them in their personal Kubernetes cluster using GitOps.

# Usage

See `examples/operator.yaml` but change the `volume` to meet your needs.

By default, the operator will reconcile secrets found in the `kubernetes` directory of your Bitwarden store. Limiting it to a folder reduces the likelyhood a `Secret` will be made with your bank credentials. Unfortunately, Bitwarden falls short in API accounts, and all accounts have full access to all features. Ideally this operator would be limited to reading secrets in the `kubernetes` directory. Maybe this is possible with organizations? I'm not sure.

You'll want to first run it with `tail -f /dev/null`, exec in, and run `bw login`. `echo -n` the token into `/root/.config/Bitwarden CLI/session`. I'm thinking of ways to make this better.

If you make a `login` secret in Bitwarden, this translates to a `Secret` with `username` and `password` keys.

If you make a `secure note` secret in Bitwarden, this translates to a `Secret` with a `notes` key, as it is described in their CLI output.

There are optional keys for `key` and `type`, which correlate to the fields on the `Secret`.

# Disclaimer

This is a hobby project. I cannot guarantee the safety of your passwords with this solution. Use at your own risk.

I'm using it in my homelab cluster to manage my secrets. It's currently satisfying my simple use cases.

I know Rust enough to write code that compiles, but not much more than that. The code style will be rough.
