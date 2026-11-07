//! Bitwarden Secrets Manager access via the official Rust SDK.

use anyhow::{Context, Result};
use bitwarden::secrets_manager::{
    AccessTokenLoginRequest, ClientSettings, SecretsManagerClient, secrets::SecretsSyncRequest,
};
use std::collections::HashMap;
use tracing::warn;
use uuid::Uuid;

/// A secret's value, as the controller turns it into `Secret` data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Value was `{"username": ..., "password": ...}`: becomes `username` and
    /// `password` keys.
    Login { username: String, password: String },
    /// Anything else: the raw value, under `spec.key` (default `notes`).
    Note(String),
}

impl Item {
    fn from_value(value: String) -> Self {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Login {
            username: String,
            password: String,
        }

        match serde_json::from_str::<Login>(&value) {
            Ok(Login { username, password }) => Self::Login { username, password },
            Err(_) => Self::Note(value),
        }
    }
}

/// A Secrets Manager client logged in as a machine account. The SDK renews the
/// short-lived bearer token from the access token on its own.
pub struct SecretsManager {
    client: SecretsManagerClient,
    organization_id: Uuid,
}

impl SecretsManager {
    pub async fn login(access_token: String) -> Result<Self> {
        let client = SecretsManagerClient::new(Some(ClientSettings {
            user_agent: concat!("bitwarden-secrets-operator/", env!("CARGO_PKG_VERSION")).into(),
            ..Default::default()
        }));
        client
            .auth()
            .login_access_token(&AccessTokenLoginRequest {
                access_token,
                state_file: None,
            })
            .await
            .context("access token login failed")?;
        let organization_id = client
            .get_access_token_organization()
            .context("access token is not tied to an organization")?
            .into();
        Ok(Self {
            client,
            organization_id,
        })
    }

    /// Every secret the machine account can read, keyed by name, in one API
    /// call. Scope comes from the machine account's project access.
    pub async fn fetch(&self) -> Result<HashMap<String, Item>> {
        let res = self
            .client
            .secrets()
            .sync(&SecretsSyncRequest {
                organization_id: self.organization_id,
                last_synced_date: None,
            })
            .await
            .context("secrets sync failed")?;

        let mut items = HashMap::new();
        for secret in res.secrets.unwrap_or_default() {
            let name = secret.key;
            if items.contains_key(&name) {
                warn!(secret = %name, "duplicate secret name, keeping the last one");
            }
            items.insert(name, Item::from_value(secret.value));
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::Item;

    fn item(v: &str) -> Item {
        Item::from_value(v.to_string())
    }

    #[test]
    fn login_json_becomes_login() {
        assert_eq!(
            item(r#"{"username":"u","password":"p"}"#),
            Item::Login {
                username: "u".into(),
                password: "p".into()
            }
        );
    }

    #[test]
    fn everything_else_is_a_note() {
        for v in [
            "plain text",
            r#"{"auths":{"ghcr.io":{"auth":"x"}}}"#, // dockerconfigjson
            r#"{"username":"u","password":"p","extra":1}"#,
            r#"{"username":"u"}"#,
            r#"{"username":1,"password":"p"}"#,
        ] {
            assert_eq!(item(v), Item::Note(v.to_string()), "{v}");
        }
    }
}
