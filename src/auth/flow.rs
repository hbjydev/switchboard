use std::time::Duration;

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use control_plane::auth::OidcProvider;
use reqwest::{Client, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{CredentialKind, Credentials, LoginOptions, WorkloadOptions, callback::Callback, now};

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

pub(super) async fn login(options: LoginOptions, allow_loopback: bool) -> Result<Credentials> {
    anyhow::ensure!(
        options
            .scopes
            .split_whitespace()
            .any(|scope| scope == "openid"),
        "browser login scopes must include openid"
    );
    let provider = OidcProvider::discover(&options.issuer, allow_loopback).await?;
    if let Some(methods) = &provider.code_challenge_methods_supported {
        anyhow::ensure!(
            methods.iter().any(|method| method == "S256"),
            "OIDC provider does not advertise PKCE S256 support"
        );
    }
    let verifier = random_secret();
    let state = random_secret();
    let nonce = random_secret();
    let mut callback = Callback::bind(options.redirect_port, state.clone()).await?;
    let url = authorization_url(
        &provider,
        &options,
        &callback.redirect_uri,
        &verifier,
        &state,
        &nonce,
    )?;
    eprintln!("Open this URL to sign in:\n{url}");
    if !options.no_browser {
        open_browser(&url).await;
    }
    let code = tokio::time::timeout(
        Duration::from_secs(options.timeout_seconds),
        callback.code(),
    )
    .await
    .context("browser login timed out")??;
    let mut form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("client_id", options.client_id.clone()),
        ("code", code),
        ("redirect_uri", callback.redirect_uri.clone()),
        ("code_verifier", verifier),
    ];
    if let Some(resource) = &options.resource {
        form.push(("resource", resource.clone()));
    }
    let response = token_request(&provider, &form, None).await?;
    let id_token = response
        .id_token
        .as_deref()
        .context("OIDC provider did not return an ID token")?;
    let identity = provider
        .validate_id_token_with_access_token(
            id_token,
            &options.client_id,
            &nonce,
            &response.access_token,
        )
        .await?;
    let access_identity = provider
        .validate_access_token(&response.access_token, &options.audience)
        .await?;
    anyhow::ensure!(
        access_identity.issuer == identity.issuer && access_identity.subject == identity.subject,
        "ID token and access token identities differ"
    );
    credentials(
        response,
        CredentialIdentity {
            issuer: provider.issuer,
            client_id: options.client_id,
            audience: options.audience,
            subject: identity.subject,
            nonce: Some(nonce),
            kind: CredentialKind::User,
            resource: options.resource,
            expires_at: access_identity.expires_at,
        },
    )
}

fn authorization_url(
    provider: &OidcProvider,
    options: &LoginOptions,
    redirect: &str,
    verifier: &str,
    state: &str,
    nonce: &str,
) -> Result<Url> {
    let mut url = Url::parse(&provider.authorization_endpoint)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", options.client_id.as_str()),
        ("redirect_uri", redirect),
        ("scope", options.scopes.as_str()),
        ("state", state),
        ("nonce", nonce),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ]);
    if let Some(resource) = &options.resource {
        url.query_pairs_mut().append_pair("resource", resource);
    }
    Ok(url)
}

pub(super) async fn workload(
    options: WorkloadOptions,
    allow_loopback: bool,
) -> Result<Credentials> {
    let provider = OidcProvider::discover(&options.issuer, allow_loopback).await?;
    let methods = provider.token_endpoint_auth_methods_supported.as_ref();
    let basic =
        methods.is_none_or(|methods| methods.iter().any(|method| method == "client_secret_basic"));
    anyhow::ensure!(
        basic
            || methods
                .is_some_and(|methods| methods.iter().any(|method| method == "client_secret_post")),
        "OIDC provider does not support client secret authentication"
    );
    let mut form = vec![
        ("grant_type", "client_credentials".to_owned()),
        ("client_id", options.client_id.clone()),
        ("scope", options.scopes),
    ];
    if !basic {
        form.push(("client_secret", options.client_secret.clone()));
    }
    if let Some(resource) = &options.resource {
        form.push(("resource", resource.clone()));
    }
    let authentication =
        basic.then_some((options.client_id.as_str(), options.client_secret.as_str()));
    let response = token_request(&provider, &form, authentication).await?;
    let identity = provider
        .validate_access_token(&response.access_token, &options.audience)
        .await?;
    anyhow::ensure!(
        identity.client_id.as_deref() == Some(options.client_id.as_str()),
        "workload token client identity differs"
    );
    credentials(
        response,
        CredentialIdentity {
            issuer: provider.issuer,
            client_id: options.client_id,
            audience: options.audience,
            subject: identity.subject,
            nonce: None,
            kind: CredentialKind::Workload,
            resource: options.resource,
            expires_at: identity.expires_at,
        },
    )
}

pub(super) async fn refresh(provider: &OidcProvider, credentials: &mut Credentials) -> Result<()> {
    let refresh = credentials
        .refresh_token
        .as_ref()
        .context("credentials expired; run switchboard auth login or auth client-credentials")?;
    anyhow::ensure!(
        matches!(credentials.kind, CredentialKind::User),
        "workload credentials expired; run switchboard auth client-credentials again"
    );
    let mut form = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("client_id", credentials.client_id.clone()),
        ("refresh_token", refresh.clone()),
    ];
    if let Some(resource) = &credentials.resource {
        form.push(("resource", resource.clone()));
    }
    let response = token_request(provider, &form, None).await?;
    if let Some(id_token) = &response.id_token {
        let nonce = credentials
            .nonce
            .as_deref()
            .context("cached login has no nonce; sign in again")?;
        let identity = provider
            .validate_refreshed_id_token(
                id_token,
                &credentials.client_id,
                nonce,
                &response.access_token,
            )
            .await?;
        anyhow::ensure!(
            identity.subject == credentials.subject,
            "refresh changed the authenticated identity; sign in again"
        );
    }
    let access_identity = provider
        .validate_access_token(&response.access_token, &credentials.audience)
        .await?;
    anyhow::ensure!(
        access_identity.subject == credentials.subject
            && access_identity.issuer == credentials.issuer,
        "refresh changed the authenticated identity; sign in again"
    );
    credentials.expires_at = token_expiry(response.expires_in, access_identity.expires_at)?;
    credentials.access_token = response.access_token;
    if response.refresh_token.is_some() {
        credentials.refresh_token = response.refresh_token;
    }
    Ok(())
}

async fn token_request(
    provider: &OidcProvider,
    form: &[(&str, String)],
    authentication: Option<(&str, &str)>,
) -> Result<TokenResponse> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut request = client.post(&provider.token_endpoint).form(form);
    if let Some((client_id, secret)) = authentication {
        request = request.basic_auth(form_encode(client_id), Some(form_encode(secret)));
    }
    let response = request
        .send()
        .await
        .map_err(|_error| anyhow::anyhow!("OIDC token endpoint is unavailable"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "OIDC token request was rejected"
    );
    let data = bounded_body(response).await?;
    let response: TokenResponse = serde_json::from_slice(&data)
        .map_err(|_error| anyhow::anyhow!("OIDC token response is invalid"))?;
    anyhow::ensure!(
        response.token_type.eq_ignore_ascii_case("bearer")
            && !response.access_token.is_empty()
            && response.expires_in.is_none_or(|expiry| expiry > 0),
        "OIDC token response is invalid"
    );
    Ok(response)
}

fn form_encode(value: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("", value)
        .finish()
        .strip_prefix('=')
        .unwrap_or_default()
        .to_owned()
}

async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_error| anyhow::anyhow!("OIDC token response could not be read"))?
    {
        anyhow::ensure!(
            bytes.len().saturating_add(chunk.len()) <= 128 * 1024,
            "OIDC token response exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

struct CredentialIdentity {
    issuer: String,
    client_id: String,
    audience: String,
    subject: String,
    nonce: Option<String>,
    kind: CredentialKind,
    resource: Option<String>,
    expires_at: u64,
}

fn credentials(response: TokenResponse, identity: CredentialIdentity) -> Result<Credentials> {
    Ok(Credentials {
        access_token: response.access_token,
        refresh_token: response.refresh_token,
        expires_at: token_expiry(response.expires_in, identity.expires_at)?,
        issuer: identity.issuer,
        client_id: identity.client_id,
        audience: identity.audience,
        subject: identity.subject,
        nonce: identity.nonce,
        kind: identity.kind,
        resource: identity.resource,
    })
}

fn token_expiry(expires_in: Option<u64>, signed_expiry: u64) -> Result<u64> {
    let current = now()?;
    let expiry = expires_in
        .map(|duration| {
            current
                .checked_add(duration)
                .context("invalid token expiry")
        })
        .transpose()?
        .unwrap_or(signed_expiry)
        .min(signed_expiry);
    anyhow::ensure!(expiry > current, "OIDC provider returned an expired token");
    Ok(expiry)
}

fn random_secret() -> String {
    // Four OS-random UUIDs provide 488 random bits, encoded without padding.
    let bytes: Vec<_> = (0..4)
        .flat_map(|_| uuid::Uuid::new_v4().as_bytes().to_vec())
        .collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

async fn open_browser(url: &Url) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    let opened = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(program)
            .arg(url.as_str())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await;
    if !matches!(opened, Ok(Ok(status)) if status.success()) {
        eprintln!("Could not launch the browser automatically; open the URL above.");
    }
}
