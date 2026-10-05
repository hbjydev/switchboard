use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::Jwk};
use reqwest::{Client, Url};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, RwLock};

use super::{AuthError, Claims, IdIdentity, TokenIdentity, token_identity};

const MAX_RESPONSE_BYTES: usize = 128 * 1024;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const KEY_TTL: Duration = Duration::from_secs(300);
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Validated discovery metadata and a bounded, shared signing-key cache.
#[derive(Clone)]
pub struct OidcProvider {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub code_challenge_methods_supported: Option<Vec<String>>,
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
    client: Client,
    jwks_uri: String,
    cache: Arc<RwLock<KeyCache>>,
    refresh: Arc<Mutex<Instant>>,
}

struct KeyCache {
    fetched: Instant,
    keys: HashMap<String, VerificationKey>,
}

#[derive(Clone)]
struct VerificationKey {
    algorithm: Algorithm,
    key: DecodingKey,
}

#[derive(Deserialize)]
struct Metadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    code_challenge_methods_supported: Option<Vec<String>>,
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct KeySet {
    keys: Vec<serde_json::Value>,
}

impl OidcProvider {
    pub async fn discover(issuer: &str, allow_loopback_http: bool) -> Result<Self, AuthError> {
        let issuer_url = validate_url(issuer, allow_loopback_http)?;
        if issuer_url.query().is_some() || issuer_url.fragment().is_some() {
            return Err(AuthError::InvalidConfig);
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_error| AuthError::ProviderUnavailable)?;
        let discovery = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let metadata: Metadata = fetch_json(&client, &discovery).await?;
        validate_metadata(&metadata, issuer, allow_loopback_http)?;
        let keys = fetch_keys(&client, &metadata.jwks_uri).await?;
        let now = Instant::now();
        Ok(Self {
            issuer: metadata.issuer,
            authorization_endpoint: metadata.authorization_endpoint,
            token_endpoint: metadata.token_endpoint,
            code_challenge_methods_supported: metadata.code_challenge_methods_supported,
            token_endpoint_auth_methods_supported: metadata.token_endpoint_auth_methods_supported,
            client,
            jwks_uri: metadata.jwks_uri,
            cache: Arc::new(RwLock::new(KeyCache { fetched: now, keys })),
            refresh: Arc::new(Mutex::new(now)),
        })
    }

    /// Readiness stops accepting keys after their bounded cache lifetime.
    pub async fn check_ready(&self) -> Result<(), AuthError> {
        self.refresh_keys(None).await
    }

    pub async fn validate_id_token(
        &self,
        token: &str,
        client_id: &str,
        nonce: &str,
    ) -> Result<IdIdentity, AuthError> {
        self.verify_id_token(token, client_id, nonce, None, true)
            .await
    }

    /// Also verify the optional OIDC access-token hash in the authorization response.
    pub async fn validate_id_token_with_access_token(
        &self,
        token: &str,
        client_id: &str,
        nonce: &str,
        access_token: &str,
    ) -> Result<IdIdentity, AuthError> {
        self.verify_id_token(token, client_id, nonce, Some(access_token), true)
            .await
    }

    /// Refresh responses may omit nonce; a supplied nonce must still match login.
    pub async fn validate_refreshed_id_token(
        &self,
        token: &str,
        client_id: &str,
        original_nonce: &str,
        access_token: &str,
    ) -> Result<IdIdentity, AuthError> {
        self.verify_id_token(token, client_id, original_nonce, Some(access_token), false)
            .await
    }

    async fn verify_id_token(
        &self,
        token: &str,
        client_id: &str,
        nonce: &str,
        access_token: Option<&str>,
        require_nonce: bool,
    ) -> Result<IdIdentity, AuthError> {
        if client_id.trim().is_empty() || nonce.is_empty() {
            return Err(AuthError::InvalidConfig);
        }
        let claims = self.verify(token, &[client_id]).await?;
        let nonce_matches = claims.nonce.as_ref().map_or(!require_nonce, |actual| {
            bool::from(actual.as_bytes().ct_eq(nonce.as_bytes()))
        });
        let authorized_party_matches = match claims.azp.as_deref() {
            Some(azp) => azp == client_id,
            None => !claims.aud.multiple(),
        };
        if !nonce_matches
            || !authorized_party_matches
            || !valid_access_hash(claims.at_hash.as_deref(), access_token)
        {
            return Err(AuthError::InvalidToken);
        }
        Ok(IdIdentity {
            issuer: claims.iss,
            subject: claims.sub,
        })
    }

    /// Validate a resource audience for CLI token caching without trusting claims.
    pub async fn validate_access_token(
        &self,
        token: &str,
        audience: &str,
    ) -> Result<TokenIdentity, AuthError> {
        if audience.trim().is_empty() {
            return Err(AuthError::InvalidConfig);
        }
        token_identity(self.verify(token, &[audience]).await?)
    }

    pub(super) async fn verify(
        &self,
        token: &str,
        audiences: &[&str],
    ) -> Result<Claims, AuthError> {
        if token.len() > MAX_TOKEN_BYTES || token.is_empty() {
            return Err(AuthError::InvalidToken);
        }
        // The untrusted header chooses only a key from this issuer's JWKS. Embedded
        // jwk/jku/x5u and algorithm-dependent remote key URLs are never followed.
        let header = decode_header(token).map_err(|_error| AuthError::InvalidToken)?;
        if !matches!(header.alg, Algorithm::RS256 | Algorithm::ES256) {
            return Err(AuthError::InvalidToken);
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| !kid.is_empty())
            .ok_or(AuthError::InvalidToken)?;
        self.refresh_keys(Some(kid)).await?;
        let key = self
            .cache
            .read()
            .await
            .keys
            .get(kid)
            .cloned()
            .ok_or(AuthError::InvalidToken)?;
        if key.algorithm != header.alg {
            return Err(AuthError::InvalidToken);
        }
        let mut validation = Validation::new(key.algorithm);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.set_audience(audiences);
        validation.leeway = 30;
        validation.validate_nbf = true;
        let claims = decode::<Claims>(token, &key.key, &validation)
            .map_err(|_error| AuthError::InvalidToken)?
            .claims;
        if claims.sub.trim().is_empty() {
            return Err(AuthError::InvalidToken);
        }
        Ok(claims)
    }

    async fn refresh_keys(&self, kid: Option<&str>) -> Result<(), AuthError> {
        if self.cache_current(kid).await {
            return Ok(());
        }
        // Serialize only refreshes. Ordinary verification reads never wait on the
        // network while a fresh known key remains cached.
        let mut last_attempt = self.refresh.lock().await;
        if self.cache_current(kid).await {
            return Ok(());
        }
        if last_attempt.elapsed() < REFRESH_INTERVAL {
            return if self.cache.read().await.fetched.elapsed() >= KEY_TTL {
                Err(AuthError::ProviderUnavailable)
            } else {
                Err(AuthError::InvalidToken)
            };
        }
        *last_attempt = Instant::now();
        let keys = fetch_keys(&self.client, &self.jwks_uri).await?;
        *self.cache.write().await = KeyCache {
            fetched: Instant::now(),
            keys,
        };
        drop(last_attempt);
        Ok(())
    }

    async fn cache_current(&self, kid: Option<&str>) -> bool {
        let cache = self.cache.read().await;
        cache.fetched.elapsed() < KEY_TTL && kid.is_none_or(|kid| cache.keys.contains_key(kid))
    }
}

fn validate_metadata(metadata: &Metadata, issuer: &str, allow_http: bool) -> Result<(), AuthError> {
    if metadata.issuer != issuer {
        return Err(AuthError::ProviderUnavailable);
    }
    for endpoint in [
        &metadata.authorization_endpoint,
        &metadata.token_endpoint,
        &metadata.jwks_uri,
    ] {
        validate_url(endpoint, allow_http).map_err(|_error| AuthError::ProviderUnavailable)?;
    }
    Ok(())
}

fn validate_url(value: &str, allow_loopback_http: bool) -> Result<Url, AuthError> {
    let url = Url::parse(value).map_err(|_error| AuthError::InvalidConfig)?;
    let authority = value
        .strip_prefix("http://")
        .and_then(|suffix| suffix.split(['/', '?', '#']).next())
        .unwrap_or_default();
    let loopback = matches!(authority, "127.0.0.1" | "[::1]")
        || authority.starts_with("127.0.0.1:")
        || authority.starts_with("[::1]:");
    let scheme_valid =
        url.scheme() == "https" || (allow_loopback_http && loopback && url.scheme() == "http");
    if !scheme_valid
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AuthError::InvalidConfig);
    }
    Ok(url)
}

fn valid_access_hash(claim: Option<&str>, access_token: Option<&str>) -> bool {
    let Some(expected) = claim else {
        return true;
    };
    let Some(access_token) = access_token else {
        return false;
    };
    let digest = Sha256::digest(access_token.as_bytes());
    let Some(first_half) = digest.get(..16) else {
        return false;
    };
    let actual = URL_SAFE_NO_PAD.encode(first_half);
    bool::from(actual.as_bytes().ct_eq(expected.as_bytes()))
}

async fn fetch_json<T: DeserializeOwned>(client: &Client, url: &str) -> Result<T, AuthError> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_error| AuthError::ProviderUnavailable)?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(AuthError::ProviderUnavailable);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_error| AuthError::ProviderUnavailable)?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(AuthError::ProviderUnavailable);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_error| AuthError::ProviderUnavailable)
}

async fn fetch_keys(
    client: &Client,
    url: &str,
) -> Result<HashMap<String, VerificationKey>, AuthError> {
    let set: KeySet = fetch_json(client, url).await?;
    parse_keys(set)
}

fn parse_keys(set: KeySet) -> Result<HashMap<String, VerificationKey>, AuthError> {
    let mut keys = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    for value in set.keys {
        let kid = value
            .get("kid")
            .and_then(serde_json::Value::as_str)
            .filter(|kid| !kid.is_empty())
            .ok_or(AuthError::ProviderUnavailable)?;
        if !seen.insert(kid.to_owned()) {
            return Err(AuthError::ProviderUnavailable);
        }
        if let Some(key) = verification_key(&value)? {
            keys.insert(kid.to_owned(), key);
        }
    }
    if keys.is_empty() {
        return Err(AuthError::ProviderUnavailable);
    }
    Ok(keys)
}

fn verification_key(value: &serde_json::Value) -> Result<Option<VerificationKey>, AuthError> {
    let algorithm = match value.get("kty").and_then(serde_json::Value::as_str) {
        Some("RSA") => Algorithm::RS256,
        Some("EC") if value.get("crv").and_then(serde_json::Value::as_str) == Some("P-256") => {
            Algorithm::ES256
        }
        _ => return Ok(None),
    };
    let expected_alg = match algorithm {
        Algorithm::RS256 => "RS256",
        _ => "ES256",
    };
    if value
        .get("alg")
        .is_some_and(|alg| alg.as_str() != Some(expected_alg))
        || value
            .get("use")
            .is_some_and(|usage| usage.as_str() != Some("sig"))
        || value.get("key_ops").is_some_and(|operations| {
            !operations.as_array().is_some_and(|operations| {
                operations.iter().all(|op| op.as_str() == Some("verify")) && !operations.is_empty()
            })
        })
    {
        return Ok(None);
    }
    let jwk: Jwk =
        serde_json::from_value(value.clone()).map_err(|_error| AuthError::ProviderUnavailable)?;
    let key = DecodingKey::from_jwk(&jwk).map_err(|_error| AuthError::ProviderUnavailable)?;
    Ok(Some(VerificationKey { algorithm, key }))
}

#[cfg(test)]
mod tests {
    use super::{
        AuthError, KEY_TTL, KeyCache, KeySet, OidcProvider, REFRESH_INTERVAL, parse_keys,
        valid_access_hash, validate_url, verification_key,
    };
    use axum::{Router, http::StatusCode, routing::get};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use serde_json::json;
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::sync::{Mutex, RwLock};

    #[test]
    fn plaintext_is_limited_to_explicit_literal_loopback() {
        validate_url("https://id.example.test", false).expect("HTTPS URL");
        for url in [
            "http://id.example.test",
            "http://localhost:8080",
            "http://127.0.0.2",
            "http://127.0.0.1.example.test",
            "http://127.1",
            "http://2130706433",
        ] {
            validate_url(url, true).expect_err("invalid plaintext URL");
        }
        validate_url("http://127.0.0.1:8080", true).expect("literal loopback URL");
        validate_url("http://[::1]:8080", true).expect("literal loopback URL");
        validate_url("http://127.0.0.1:8080", false).expect_err("invalid URL");
        validate_url("https://user:secret@id.example.test", false).expect_err("invalid URL");
        validate_url("https://id.example.test/#fragment", false).expect_err("invalid URL");
    }

    #[test]
    fn oidc_at_hash_requires_the_exact_access_token() {
        assert!(valid_access_hash(None, None));
        // SHA-256 left half, base64url, as required for both allowed signing algorithms.
        let hash = "LPJNul-wow4m6Dsqxbning";
        assert!(valid_access_hash(Some(hash), Some("hello")));
        assert!(!valid_access_hash(Some(hash), Some("goodbye")));
        assert!(!valid_access_hash(Some(hash), None));
    }

    #[test]
    fn signing_keys_must_match_algorithm_and_usage() {
        for key in [
            json!({"kty":"oct", "alg":"HS256"}),
            json!({"kty":"RSA", "alg":"HS256"}),
            json!({"kty":"EC", "crv":"P-256", "alg":"RS256"}),
            json!({"kty":"EC", "crv":"P-384"}),
            json!({"kty":"RSA", "use":"enc"}),
            json!({"kty":"RSA", "key_ops":["sign"]}),
            json!({"kty":"RSA", "key_ops":[]}),
            json!({"kty":"RSA", "key_ops":"verify"}),
        ] {
            assert!(verification_key(&key).expect("key filter").is_none());
        }
    }

    #[test]
    fn discovery_metadata_is_bound_to_the_configured_issuer() {
        let issuer = "https://id.example.test";
        let mut metadata = super::Metadata {
            issuer: issuer.into(),
            authorization_endpoint: format!("{issuer}/authorize"),
            token_endpoint: format!("{issuer}/token"),
            jwks_uri: format!("{issuer}/keys"),
            code_challenge_methods_supported: None,
            token_endpoint_auth_methods_supported: None,
        };
        super::validate_metadata(&metadata, issuer, false).expect("bound metadata");
        metadata.issuer = "https://other-issuer.example.test".into();
        super::validate_metadata(&metadata, issuer, false).expect_err("issuer mismatch");
        metadata.issuer = issuer.into();
        metadata.jwks_uri = "http://10.0.0.1/keys".into();
        super::validate_metadata(&metadata, issuer, true).expect_err("unsafe issuer key endpoint");
        metadata.jwks_uri = "https://user:secret@id.example.test/keys".into();
        super::validate_metadata(&metadata, issuer, false)
            .expect_err("credentials in endpoint URL");
    }

    #[tokio::test]
    async fn expired_keys_are_never_used_when_refresh_fails() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener");
        let issuer = format!("http://{}", listener.local_addr().expect("test address"));
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/keys", get(|| async { StatusCode::SERVICE_UNAVAILABLE })),
            )
            .await
            .expect("test issuer");
        });
        let key =
            serde_json::from_str(include_str!("../../../../tests/support/keys/oidc.jwk.json"))
                .expect("fixture JWK");
        let keys = parse_keys(KeySet { keys: vec![key] }).expect("fixture key set");
        let provider = OidcProvider {
            issuer: issuer.clone(),
            authorization_endpoint: format!("{issuer}/authorize"),
            token_endpoint: format!("{issuer}/token"),
            code_challenge_methods_supported: None,
            token_endpoint_auth_methods_supported: None,
            client: reqwest::Client::new(),
            jwks_uri: format!("{issuer}/keys"),
            cache: Arc::new(RwLock::new(KeyCache {
                fetched: Instant::now()
                    .checked_sub(KEY_TTL + Duration::from_secs(1))
                    .expect("past instant"),
                keys,
            })),
            refresh: Arc::new(Mutex::new(
                Instant::now()
                    .checked_sub(REFRESH_INTERVAL)
                    .expect("past refresh"),
            )),
        };
        let readiness = provider.check_ready().await;
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"fixture"}"#);
        let verification = provider
            .verify(&format!("{header}.e30.signature"), &["users"])
            .await;
        server.abort();
        assert!(matches!(readiness, Err(AuthError::ProviderUnavailable)));
        assert!(matches!(verification, Err(AuthError::ProviderUnavailable)));
    }
}
