//! Provider-neutral OAuth resource-server and OIDC client validation.
mod provider;

use std::collections::BTreeSet;

use serde::Deserialize;
use thiserror::Error;

pub use provider::OidcProvider;

/// Deployment configuration; audiences distinguish users from machine clients.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub issuer: String,
    pub user_audience: String,
    pub workload_audience: Option<String>,
    pub allow_loopback_http: bool,
}

/// Sanitized errors, safe to translate into API or CLI diagnostics.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid OIDC configuration")]
    InvalidConfig,
    #[error("OIDC provider is unavailable or returned invalid metadata")]
    ProviderUnavailable,
    #[error("invalid or expired token")]
    InvalidToken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrincipalKind {
    User,
    Workload,
}

/// Identity comes only from signed claims, never request-body actor names.
#[derive(Clone, Debug)]
pub struct Principal {
    pub issuer: String,
    pub subject: String,
    pub kind: PrincipalKind,
    pub scopes: BTreeSet<String>,
    pub client_id: Option<String>,
}

impl Principal {
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }
}

/// Verified ID-token identity for the browser-login client.
#[derive(Debug)]
pub struct IdIdentity {
    pub issuer: String,
    pub subject: String,
}

/// Verified resource-token claims; audience configuration alone defines roles.
#[derive(Debug)]
pub struct TokenIdentity {
    pub issuer: String,
    pub subject: String,
    pub client_id: Option<String>,
    pub scopes: BTreeSet<String>,
    pub expires_at: u64,
}

#[derive(Clone)]
pub struct Authenticator {
    provider: OidcProvider,
    config: AuthConfig,
}

impl Authenticator {
    pub async fn discover(config: AuthConfig) -> Result<Self, AuthError> {
        validate_audiences(&config)?;
        let provider = OidcProvider::discover(&config.issuer, config.allow_loopback_http).await?;
        Ok(Self { provider, config })
    }

    #[must_use]
    pub const fn provider(&self) -> &OidcProvider {
        &self.provider
    }

    pub async fn check_ready(&self) -> Result<(), AuthError> {
        self.provider.check_ready().await
    }

    pub async fn authenticate(&self, token: &str) -> Result<Principal, AuthError> {
        let mut audiences = vec![self.config.user_audience.as_str()];
        if let Some(workload) = &self.config.workload_audience {
            audiences.push(workload);
        }
        let claims = self.provider.verify(token, &audiences).await?;
        let user = claims.aud.contains(&self.config.user_audience);
        let workload = self
            .config
            .workload_audience
            .as_ref()
            .is_some_and(|aud| claims.aud.contains(aud));
        let kind = match (user, workload) {
            (true, false) => PrincipalKind::User,
            (false, true) => PrincipalKind::Workload,
            _ => return Err(AuthError::InvalidToken),
        };
        principal(claims, kind)
    }
}

fn validate_audiences(config: &AuthConfig) -> Result<(), AuthError> {
    if config.user_audience.trim().is_empty()
        || config
            .workload_audience
            .as_ref()
            .is_some_and(|aud| aud.trim().is_empty() || aud == &config.user_audience)
    {
        return Err(AuthError::InvalidConfig);
    }
    Ok(())
}

pub(super) fn principal(claims: Claims, kind: PrincipalKind) -> Result<Principal, AuthError> {
    let identity = token_identity(claims)?;
    if kind == PrincipalKind::Workload && identity.client_id.is_none() {
        return Err(AuthError::InvalidToken);
    }
    Ok(Principal {
        issuer: identity.issuer,
        subject: identity.subject,
        kind,
        scopes: identity.scopes,
        client_id: identity.client_id,
    })
}

pub(super) fn token_identity(claims: Claims) -> Result<TokenIdentity, AuthError> {
    let client_id = claims.client_id.as_ref().or(claims.azp.as_ref()).cloned();
    if claims
        .client_id
        .as_ref()
        .zip(claims.azp.as_ref())
        .is_some_and(|(client, azp)| client != azp)
        || client_id
            .as_ref()
            .is_some_and(|client| client.trim().is_empty())
    {
        return Err(AuthError::InvalidToken);
    }
    let mut scopes: BTreeSet<String> = claims
        .scope
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if let Some(scp) = claims.scp {
        match scp {
            ScopeClaim::String(value) => scopes.extend(value.split_whitespace().map(str::to_owned)),
            ScopeClaim::List(values) => scopes.extend(values),
        }
    }
    Ok(TokenIdentity {
        issuer: claims.iss,
        subject: claims.sub,
        scopes,
        client_id,
        expires_at: claims.exp,
    })
}

#[derive(Deserialize)]
pub(super) struct Claims {
    pub iss: String,
    pub sub: String,
    pub aud: Audience,
    // Registered temporal claims are checked by jsonwebtoken's Validation.
    pub exp: u64,
    pub nonce: Option<String>,
    pub azp: Option<String>,
    pub client_id: Option<String>,
    pub scope: Option<String>,
    pub scp: Option<ScopeClaim>,
    pub at_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Audience {
    String(String),
    List(Vec<String>),
}

impl Audience {
    pub fn contains(&self, expected: &str) -> bool {
        match self {
            Self::String(aud) => aud == expected,
            Self::List(aud) => aud.iter().any(|value| value == expected),
        }
    }

    pub const fn multiple(&self) -> bool {
        matches!(self, Self::List(values) if values.len() > 1)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum ScopeClaim {
    String(String),
    List(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::{AuthConfig, Claims, PrincipalKind, principal, validate_audiences};
    use serde_json::json;

    #[test]
    fn audiences_must_be_nonempty_and_distinct() {
        let mut config = AuthConfig {
            issuer: "https://issuer.example.test".into(),
            user_audience: "users".into(),
            workload_audience: Some("workers".into()),
            allow_loopback_http: false,
        };
        validate_audiences(&config).expect("distinct audiences");
        config.workload_audience = Some("users".into());
        validate_audiences(&config).expect_err("invalid audiences");
        config.workload_audience = Some(" ".into());
        validate_audiences(&config).expect_err("invalid audiences");
        config.workload_audience = None;
        config.user_audience.clear();
        validate_audiences(&config).expect_err("invalid audiences");
    }

    #[test]
    fn signed_scope_claim_formats_are_combined() {
        let claims: Claims = serde_json::from_value(json!({
            "iss": "https://issuer.example.test", "sub": "user", "aud": "users", "exp": 1,
            "scope": "switchboard:read switchboard:write", "scp": ["custom:scope"]
        }))
        .expect("claim fixture");
        let actor = principal(claims, PrincipalKind::User).expect("principal");
        assert!(actor.has_scope("switchboard:read"));
        assert!(actor.has_scope("switchboard:write"));
        assert!(actor.has_scope("custom:scope"));
        assert!(!actor.has_scope("admin"));
    }

    #[test]
    fn workloads_require_unambiguous_client_identity() {
        for extra in [
            json!({}),
            json!({"client_id": " "}),
            json!({"client_id": "one", "azp": "two"}),
        ] {
            let mut value = json!({"iss":"https://issuer.example.test", "sub":"worker", "aud":"workers", "exp":1});
            value
                .as_object_mut()
                .expect("fixture object")
                .extend(extra.as_object().expect("extra object").clone());
            let claims: Claims = serde_json::from_value(value).expect("claims");
            principal(claims, PrincipalKind::Workload).expect_err("ambiguous workload identity");
        }
        let claims: Claims = serde_json::from_value(json!({
            "iss":"https://issuer.example.test", "sub":"worker", "aud":"workers", "exp":1, "azp":"remote-worker"
        })).expect("claims");
        let actor = principal(claims, PrincipalKind::Workload).expect("workload");
        assert_eq!(actor.client_id.as_deref(), Some("remote-worker"));
    }
}
