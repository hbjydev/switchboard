//! Public-client OIDC login and private OAuth credential cache.
mod cache;
mod callback;
mod flow;

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use control_plane::auth::OidcProvider;
use serde::{Deserialize, Serialize};

#[derive(Args)]
pub struct ClientAuthOptions {
    #[arg(
        long,
        global = true,
        env = "SWITCHBOARD_ACCESS_TOKEN",
        hide_env_values = true
    )]
    access_token: Option<String>,
    #[arg(long, global = true, env = "SWITCHBOARD_CREDENTIALS_FILE")]
    credentials_file: Option<PathBuf>,
    /// Permit HTTP only for literal loopback OIDC providers, for local testing.
    #[arg(
        long,
        global = true,
        env = "SWITCHBOARD_OIDC_ALLOW_LOOPBACK_HTTP",
        default_value_t = false
    )]
    pub oidc_allow_loopback_http: bool,
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Sign in through your OIDC provider using a browser and PKCE.
    Login(LoginOptions),
    /// Obtain a workload token using an `OAuth2` confidential client.
    ClientCredentials(WorkloadOptions),
    /// Remove locally cached credentials (does not revoke provider sessions).
    Logout,
}

#[derive(Args)]
pub struct LoginOptions {
    #[arg(long, env = "SWITCHBOARD_OIDC_ISSUER")]
    issuer: String,
    #[arg(long, env = "SWITCHBOARD_OIDC_CLIENT_ID")]
    client_id: String,
    #[arg(long, env = "SWITCHBOARD_OIDC_USER_AUDIENCE")]
    audience: String,
    #[arg(long, default_value_t = 8400)]
    redirect_port: u16,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=1800))]
    timeout_seconds: u64,
    #[arg(
        long,
        env = "SWITCHBOARD_OAUTH_SCOPES",
        default_value = "openid profile offline_access switchboard:read switchboard:write"
    )]
    scopes: String,
    #[arg(long, env = "SWITCHBOARD_OAUTH_RESOURCE")]
    resource: Option<String>,
    #[arg(long)]
    no_browser: bool,
}

#[derive(Args)]
pub struct WorkloadOptions {
    #[arg(long, env = "SWITCHBOARD_OIDC_ISSUER")]
    issuer: String,
    #[arg(long, env = "SWITCHBOARD_OAUTH_CLIENT_ID")]
    client_id: String,
    #[arg(long, env = "SWITCHBOARD_OAUTH_CLIENT_SECRET", hide_env_values = true)]
    client_secret: String,
    #[arg(long, env = "SWITCHBOARD_OIDC_WORKLOAD_AUDIENCE")]
    audience: String,
    #[arg(
        long,
        env = "SWITCHBOARD_OAUTH_SCOPES",
        default_value = "switchboard:read"
    )]
    scopes: String,
    #[arg(long, env = "SWITCHBOARD_OAUTH_RESOURCE")]
    resource: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CredentialKind {
    User,
    Workload,
}

#[derive(Serialize, Deserialize)]
struct Credentials {
    access_token: String,
    refresh_token: Option<String>,
    issuer: String,
    client_id: String,
    audience: String,
    expires_at: u64,
    subject: String,
    nonce: Option<String>,
    kind: CredentialKind,
    resource: Option<String>,
}

impl ClientAuthOptions {
    pub async fn access_token(&self) -> Result<String> {
        if let Some(token) = &self.access_token {
            anyhow::ensure!(
                !token.is_empty(),
                "SWITCHBOARD_ACCESS_TOKEN must not be empty"
            );
            return Ok(token.clone());
        }
        let path = self.credentials_path()?;
        let mut credentials = cache::read(&path).context("no usable cached credentials; run switchboard auth login or configure SWITCHBOARD_ACCESS_TOKEN")?;
        let current = now()?;
        let refreshable =
            credentials.refresh_token.is_some() && matches!(credentials.kind, CredentialKind::User);
        if refreshable && credentials.expires_at <= current.saturating_add(30) {
            let provider =
                OidcProvider::discover(&credentials.issuer, self.oidc_allow_loopback_http).await?;
            flow::refresh(&provider, &mut credentials).await?;
            cache::write(&path, &credentials)?;
        }
        anyhow::ensure!(
            credentials.expires_at > current,
            "credentials expired; run switchboard auth login or auth client-credentials"
        );
        Ok(credentials.access_token)
    }

    pub async fn execute(&self, command: AuthCommand) -> Result<()> {
        let path = self.credentials_path()?;
        match command {
            AuthCommand::Login(options) => {
                let credentials = flow::login(options, self.oidc_allow_loopback_http).await?;
                cache::write(&path, &credentials)?;
                println!("Signed in. Credentials saved to {}", path.display());
            }
            AuthCommand::ClientCredentials(options) => {
                let credentials = flow::workload(options, self.oidc_allow_loopback_http).await?;
                cache::write(&path, &credentials)?;
                println!("Workload credentials saved to {}", path.display());
            }
            AuthCommand::Logout => {
                cache::remove(&path)?;
                println!("Local credentials removed");
            }
        }
        Ok(())
    }

    fn credentials_path(&self) -> Result<PathBuf> {
        if let Some(path) = &self.credentials_file {
            return Ok(path.clone());
        }
        if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
            let state = Path::new(&state);
            anyhow::ensure!(
                state.is_absolute(),
                "XDG_STATE_HOME must be an absolute path"
            );
            return Ok(state.join("switchboard/credentials.json"));
        }
        let home = std::env::var_os("HOME")
            .context("set SWITCHBOARD_CREDENTIALS_FILE or HOME for credential storage")?;
        Ok(Path::new(&home).join(".local/state/switchboard/credentials.json"))
    }
}

fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}
