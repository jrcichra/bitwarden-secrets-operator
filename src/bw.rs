use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::process::Command;
use tracing::{info, warn};

/// Thin wrapper around the `bw` CLI. Holds the current session token and
/// transparently re-logs-in when a call fails because the session expired.
pub struct BitwardenClient {
    session: Mutex<String>,
}

impl BitwardenClient {
    pub fn new(session: String) -> Self {
        Self {
            session: Mutex::new(session),
        }
    }

    /// Log in to Bitwarden from scratch using the BW_CLIENTID / BW_CLIENTSECRET /
    /// BW_PASSWORD env vars, returning a fresh session token.
    pub async fn login() -> Result<String> {
        Command::new("bw").arg("logout").output().await?;

        let res = Command::new("bw")
            .args(["login", "--apikey"])
            .output()
            .await?;
        if !res.status.success() {
            anyhow::bail!(
                "login failed: stdout: {}\nstderr: {}",
                String::from_utf8_lossy(&res.stdout),
                String::from_utf8_lossy(&res.stderr)
            );
        }

        // TODO: this may hang if BW_PASSWORD is not set
        let res = Command::new("bw")
            .args(["unlock", "--passwordenv", "BW_PASSWORD"])
            .output()
            .await?;
        let stdout = String::from_utf8_lossy(&res.stdout);
        if !res.status.success() {
            anyhow::bail!(
                "unlock failed: stdout: {}\nstderr: {}",
                stdout,
                String::from_utf8_lossy(&res.stderr)
            );
        }

        lazy_static::lazy_static! {
            static ref RE: Regex = Regex::new("export BW_SESSION=\"(.*)\"").unwrap();
        }
        if let Some(cap) = RE.captures_iter(&stdout).next() {
            return Ok(cap[1].to_string());
        }

        anyhow::bail!("could not find BW_SESSION in unlock output")
    }

    /// Fetch all items in `folder`, re-logging in transparently if the current
    /// session has expired.
    pub async fn get_secrets(&self, folder: &str) -> Result<HashMap<String, Value>> {
        let current_session = self.session.lock().unwrap().clone();
        match Self::fetch_secrets(&current_session, folder).await {
            Ok(secrets) => Ok(secrets),
            Err(e) => {
                warn!("cache refresh failed ({}), re-logging in...", e);
                let new_session = Self::login().await?;
                let secrets = Self::fetch_secrets(&new_session, folder).await?;
                *self.session.lock().unwrap() = new_session;
                Ok(secrets)
            }
        }
    }

    // Collect secrets for a single session attempt and return a hash of the results.
    async fn fetch_secrets(session: &str, folder: &str) -> Result<HashMap<String, Value>> {
        let mut secrets = HashMap::new();
        info!("loading secrets from folder '{}'", folder);

        // Pass the session via env var rather than argv so it doesn't show up in
        // /proc/<pid>/cmdline for other processes on the host to read.
        let res = Command::new("bw")
            .args(["sync"])
            .env("BW_SESSION", session)
            .output()
            .await?;
        if !res.status.success() {
            anyhow::bail!(
                "sync failed: stdout: {}\nstderr: {}",
                String::from_utf8_lossy(&res.stdout),
                String::from_utf8_lossy(&res.stderr)
            );
        }

        let res = Command::new("bw")
            .args(["get", "folder", folder])
            .env("BW_SESSION", session)
            .output()
            .await?;
        if !res.status.success() {
            anyhow::bail!(
                "get folder failed: stdout: {}\nstderr: {}",
                String::from_utf8_lossy(&res.stdout),
                String::from_utf8_lossy(&res.stderr)
            );
        }
        let folder_json: Value = serde_json::from_slice(&res.stdout).with_context(|| {
            format!(
                "get folder returned non-JSON (stdout: {:?}, stderr: {:?})",
                String::from_utf8_lossy(&res.stdout),
                String::from_utf8_lossy(&res.stderr)
            )
        })?;

        let folder_id = folder_json
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "folder '{}' not found or has no id (bw output: {})",
                    folder,
                    folder_json
                )
            })?;

        let res = Command::new("bw")
            .args(["list", "items", "--folderid", folder_id])
            .env("BW_SESSION", session)
            .output()
            .await?;
        if !res.status.success() {
            anyhow::bail!(
                "list items failed: stdout: {}\nstderr: {}",
                String::from_utf8_lossy(&res.stdout),
                String::from_utf8_lossy(&res.stderr)
            );
        }
        let v: Value = serde_json::from_slice(&res.stdout).with_context(|| {
            format!(
                "list items returned non-JSON (stdout: {:?}, stderr: {:?})",
                String::from_utf8_lossy(&res.stdout),
                String::from_utf8_lossy(&res.stderr)
            )
        })?;

        if let Some(arr) = v.as_array() {
            for item in arr {
                if let Some(name) = item["name"].as_str() {
                    secrets.insert(name.to_string(), item.clone());
                }
            }
        }

        info!("loaded {} secrets from folder '{}'", secrets.len(), folder);
        Ok(secrets)
    }
}
