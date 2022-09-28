# bitwarden-secrets-operator

A Rust Kubernetes operator that reconciles `BitwardenSecrets` into `Secrets`.

In other words, you can reference Bitwarden passwords from Kubernetes natively through `Secrets`.

# Usage

See `examples/operator.yaml`. You'll want to first run it with `tail -f /dev/null`, exec in, and run `bw login`. `echo -n` the token into `/root/.config/Bitwarden CLI/session`.

The example uses a PVC with `ReadWriteOnce` but if your storage class supports it, you can change that to `ReadWriteMany` and up the replica count. There's currently no leader election, but that should be OK.

# Disclaimer

This is a hobby project. I cannot guarantee the safety of your passwords with this solution. Use at your own risk.

I know Rust enough to write code that compiles, but not much more than that. The code style will be rough.
