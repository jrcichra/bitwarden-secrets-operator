# bitwarden-secrets-operator

A Rust Kubernetes operator that reconciles `BitwardenSecrets` into `Secrets`.

In other words, you can reference Bitwarden passwords from Kubernetes natively through `Secrets`.

# Disclaimer

This is a hobby project. I cannot guarantee the safety of your passwords with this solution. Use at your own risk.

I know Rust enough to write code that compiles, but not much more than that. The code style will be rough.
