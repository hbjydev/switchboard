#[path = "../../../tests/support/oidc.rs"]
pub mod oidc;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use control_plane::auth::{AuthConfig, AuthError, Authenticator, OidcProvider, PrincipalKind};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use oidc::{CLI_ID, TestIssuer, USER_AUDIENCE, WORKLOAD_AUDIENCE};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_secs()
}

fn signed_header(claims: &Value, header: &Header) -> String {
    let key = EncodingKey::from_rsa_pem(include_bytes!("../../../tests/support/keys/oidc.pem"))
        .expect("fixture key");
    encode(header, claims, &key).expect("signed fixture")
}

#[tokio::test]
async fn user_and_workload_access_tokens_have_verified_identities_and_scopes() {
    let issuer = TestIssuer::start().await;
    let auth = issuer.authenticator().await;
    let user = auth
        .authenticate(&issuer.user_token("alice", "switchboard:read switchboard:write"))
        .await
        .expect("user");
    assert_eq!(user.issuer, issuer.issuer);
    assert_eq!(user.subject, "alice");
    assert_eq!(user.kind, PrincipalKind::User);
    assert!(user.has_scope("switchboard:write"));
    let workload = auth
        .authenticate(&issuer.workload_token("remote-agent", "switchboard:read"))
        .await
        .expect("workload");
    assert_eq!(workload.kind, PrincipalKind::Workload);
    assert_eq!(workload.client_id.as_deref(), Some("remote-agent"));
    assert!(!workload.has_scope("switchboard:write"));
    auth.check_ready().await.expect("fresh issuer keys");
}

#[tokio::test]
async fn invalid_registered_claims_are_rejected() {
    let issuer = TestIssuer::start().await;
    let auth = issuer.authenticator().await;
    for (claim, invalid) in [
        ("iss", json!("https://other-issuer.example.test")),
        ("aud", json!(CLI_ID)),
        ("exp", json!(now().saturating_sub(120))),
        ("nbf", json!(now() + 120)),
        ("sub", json!(" ")),
        ("sub", json!(17)),
        ("exp", json!("not-a-timestamp")),
    ] {
        let mut claims = issuer.user_claims("alice", "switchboard:read");
        claims[claim] = invalid;
        assert!(
            matches!(
                auth.authenticate(&issuer.sign(&claims)).await,
                Err(AuthError::InvalidToken)
            ),
            "claim {claim}"
        );
    }
    for required in ["exp", "iss", "aud", "sub"] {
        let mut claims = issuer.user_claims("alice", "switchboard:read");
        claims.as_object_mut().expect("claims").remove(required);
        assert!(
            matches!(
                auth.authenticate(&issuer.sign(&claims)).await,
                Err(AuthError::InvalidToken)
            ),
            "required claim {required}"
        );
    }
}

#[tokio::test]
async fn signatures_algorithm_and_kid_are_required() {
    let issuer = TestIssuer::start().await;
    let auth = issuer.authenticator().await;
    let claims = issuer.user_claims("alice", "switchboard:read");
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("fixture".into());
    let forged = encode(
        &header,
        &claims,
        &EncodingKey::from_secret(b"public-key-is-not-an-hmac-secret"),
    )
    .expect("forged fixture");
    assert!(matches!(
        auth.authenticate(&forged).await,
        Err(AuthError::InvalidToken)
    ));
    assert!(matches!(
        auth.authenticate(&signed_header(&claims, &Header::new(Algorithm::RS256)))
            .await,
        Err(AuthError::InvalidToken)
    ));
    assert!(matches!(
        auth.authenticate(&issuer.sign_with_kid(&claims.clone(), ""))
            .await,
        Err(AuthError::InvalidToken)
    ));
    assert!(matches!(
        auth.authenticate(&issuer.sign_with_kid(&claims, "unknown"))
            .await,
        Err(AuthError::InvalidToken)
    ));
    assert!(matches!(
        auth.authenticate(&"x".repeat(16 * 1024 + 1)).await,
        Err(AuthError::InvalidToken)
    ));
    let valid = issuer.user_token("alice", "switchboard:read");
    let mut segments: Vec<_> = valid.split('.').map(str::to_owned).collect();
    let mut changed = issuer.user_claims("attacker", "switchboard:write");
    changed["sub"] = json!("attacker");
    segments[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&changed).expect("claims bytes"));
    assert!(matches!(
        auth.authenticate(&segments.join(".")).await,
        Err(AuthError::InvalidToken)
    ));
}

#[tokio::test]
async fn embedded_remote_key_urls_are_not_followed() {
    let issuer = TestIssuer::start().await;
    let auth = issuer.authenticator().await;
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("fixture".into());
    header.jku = Some("http://127.0.0.1:1/untrusted-jwks".into());
    header.x5u = Some("http://127.0.0.1:1/untrusted-cert".into());
    let token = signed_header(&issuer.user_claims("alice", "switchboard:read"), &header);
    let principal = auth
        .authenticate(&token)
        .await
        .expect("configured issuer key only");
    assert_eq!(principal.subject, "alice");
}

#[tokio::test]
async fn scopes_and_workload_identity_cannot_be_confused() {
    let issuer = TestIssuer::start().await;
    let auth = issuer.authenticator().await;
    let mut claims = issuer.user_claims("alice", "");
    claims["scp"] = json!(["switchboard:read", "switchboard:write"]);
    assert!(
        auth.authenticate(&issuer.sign(&claims))
            .await
            .expect("scp array")
            .has_scope("switchboard:write")
    );
    let mut claims = issuer.user_claims("alice", "");
    claims["scp"] = json!("switchboard:read");
    assert!(
        auth.authenticate(&issuer.sign(&claims))
            .await
            .expect("scp string")
            .has_scope("switchboard:read")
    );
    let mut claims = issuer.workload_claims("remote-agent", "switchboard:read");
    claims.as_object_mut().expect("claims").remove("client_id");
    assert!(matches!(
        auth.authenticate(&issuer.sign(&claims.clone())).await,
        Err(AuthError::InvalidToken)
    ));
    claims["azp"] = json!("remote-agent");
    assert_eq!(
        auth.authenticate(&issuer.sign(&claims.clone()))
            .await
            .expect("azp workload")
            .kind,
        PrincipalKind::Workload
    );
    claims["client_id"] = json!("another-client");
    assert!(matches!(
        auth.authenticate(&issuer.sign(&claims)).await,
        Err(AuthError::InvalidToken)
    ));
    let mut claims = issuer.user_claims("alice", "switchboard:read");
    claims["aud"] = json!([USER_AUDIENCE, WORKLOAD_AUDIENCE]);
    assert!(matches!(
        auth.authenticate(&issuer.sign(&claims)).await,
        Err(AuthError::InvalidToken)
    ));
}

#[tokio::test]
async fn browser_id_tokens_validate_nonce_authorized_party_and_access_hash() {
    let issuer = TestIssuer::start().await;
    let provider = OidcProvider::discover(&issuer.issuer, true)
        .await
        .expect("provider");
    let access = issuer.user_token("alice", "switchboard:read");
    let mut claims = issuer.user_claims("alice", "openid");
    claims["aud"] = json!(CLI_ID);
    claims["nonce"] = json!("login-nonce");
    let token = issuer.sign(&claims.clone());
    assert_eq!(
        provider
            .validate_id_token(&token, CLI_ID, "login-nonce")
            .await
            .expect("ID identity")
            .subject,
        "alice"
    );
    provider
        .validate_id_token(&token, CLI_ID, "wrong-nonce")
        .await
        .expect_err("invalid ID token");
    provider
        .validate_id_token(&token, USER_AUDIENCE, "login-nonce")
        .await
        .expect_err("invalid ID token");
    claims["aud"] = json!([CLI_ID, "another-client"]);
    provider
        .validate_id_token(&issuer.sign(&claims.clone()), CLI_ID, "login-nonce")
        .await
        .expect_err("invalid ID token");
    claims["azp"] = json!(CLI_ID);
    provider
        .validate_id_token(&issuer.sign(&claims.clone()), CLI_ID, "login-nonce")
        .await
        .expect("multi audience authorized party");
    claims["azp"] = json!("another-client");
    provider
        .validate_id_token(&issuer.sign(&claims.clone()), CLI_ID, "login-nonce")
        .await
        .expect_err("invalid ID token");
    claims["azp"] = json!(CLI_ID);
    let digest = Sha256::digest(access.as_bytes());
    claims["at_hash"] = json!(URL_SAFE_NO_PAD.encode(digest.get(..16).expect("half hash")));
    let token = issuer.sign(&claims);
    provider
        .validate_id_token_with_access_token(&token, CLI_ID, "login-nonce", &access)
        .await
        .expect("valid hash");
    provider
        .validate_id_token_with_access_token(&token, CLI_ID, "login-nonce", "wrong-access-token")
        .await
        .expect_err("invalid ID token");
    provider
        .validate_id_token(&token, CLI_ID, "login-nonce")
        .await
        .expect_err("invalid ID token");
    provider
        .validate_access_token(&token, USER_AUDIENCE)
        .await
        .expect_err("invalid ID token");
}

#[tokio::test]
async fn refreshed_id_tokens_may_omit_nonce_but_cannot_change_it() {
    let issuer = TestIssuer::start().await;
    let provider = OidcProvider::discover(&issuer.issuer, true)
        .await
        .expect("provider");
    let access = issuer.user_token("alice", "switchboard:read");
    let mut claims = issuer.user_claims("alice", "openid");
    claims["aud"] = json!(CLI_ID);
    let token = issuer.sign(&claims);
    provider
        .validate_refreshed_id_token(&token, CLI_ID, "original-nonce", &access)
        .await
        .expect("omitted refresh nonce");
    provider
        .validate_id_token_with_access_token(&token, CLI_ID, "original-nonce", &access)
        .await
        .expect_err("initial login requires nonce");
    claims["nonce"] = json!("original-nonce");
    provider
        .validate_refreshed_id_token(&issuer.sign(&claims), CLI_ID, "original-nonce", &access)
        .await
        .expect("matching refresh nonce");
    claims["nonce"] = json!("another-login");
    provider
        .validate_refreshed_id_token(&issuer.sign(&claims), CLI_ID, "original-nonce", &access)
        .await
        .expect_err("different refresh nonce");
}

#[tokio::test]
async fn resource_tokens_may_include_provider_extension_claims() {
    let issuer = TestIssuer::start().await;
    let auth = issuer.authenticator().await;
    for (claim, value) in [
        ("nonce", json!("login-nonce")),
        ("at_hash", json!("login-access-hash")),
    ] {
        let mut claims = issuer.user_claims("alice", "switchboard:read switchboard:write");
        claims[claim] = value;
        let token = issuer.sign(&claims);
        assert_eq!(
            auth.authenticate(&token)
                .await
                .expect("provider extension")
                .subject,
            "alice"
        );
        auth.provider()
            .validate_access_token(&token, USER_AUDIENCE)
            .await
            .expect("verified resource audience with extensions");
    }
}

#[tokio::test]
async fn duplicate_key_ids_and_invalid_key_usage_fail_discovery() {
    let issuer = TestIssuer::start().await;
    let jwks = issuer.jwks();
    let key = jwks["keys"][0].clone();
    issuer.set_jwks(json!({"keys":[key.clone(), key.clone()]}));
    assert!(matches!(
        OidcProvider::discover(&issuer.issuer, true).await,
        Err(AuthError::ProviderUnavailable)
    ));
    for (property, invalid) in [
        ("use", json!("enc")),
        ("alg", json!("HS256")),
        ("key_ops", json!(["sign"])),
    ] {
        let mut key = key.clone();
        key[property] = invalid;
        issuer.set_jwks(json!({"keys":[key]}));
        assert!(matches!(
            OidcProvider::discover(&issuer.issuer, true).await,
            Err(AuthError::ProviderUnavailable)
        ));
    }
}

#[tokio::test]
async fn unknown_key_rotation_refreshes_after_bounded_retry_interval() {
    let issuer = TestIssuer::start().await;
    let auth = issuer.authenticator().await;
    let token = issuer.sign_with_kid(&issuer.user_claims("alice", "switchboard:read"), "rotated");
    let mut jwks = issuer.jwks();
    jwks["keys"][0]["kid"] = json!("rotated");
    issuer.set_jwks(jwks);
    assert!(matches!(
        auth.authenticate(&token).await,
        Err(AuthError::InvalidToken)
    ));
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        auth.authenticate(&token)
            .await
            .expect("refreshed key")
            .subject,
        "alice"
    );
    issuer.set_jwks(json!({"keys":[]}));
    assert_eq!(
        auth.authenticate(&token)
            .await
            .expect("fresh cached signing key")
            .subject,
        "alice"
    );
}

#[tokio::test]
async fn es256_signatures_work_with_matching_p256_keys() {
    let issuer = TestIssuer::start().await;
    let key: Value =
        serde_json::from_str(include_str!("support/es256.jwk.json")).expect("EC public fixture");
    issuer.set_jwks(json!({"keys":[key]}));
    let auth = issuer.authenticator().await;
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("ec-fixture".into());
    let key =
        EncodingKey::from_ec_pem(include_bytes!("support/es256.pem")).expect("EC private fixture");
    let token = encode(
        &header,
        &issuer.user_claims("alice", "switchboard:read"),
        &key,
    )
    .expect("EC token");
    assert_eq!(
        auth.authenticate(&token)
            .await
            .expect("ES256 verification")
            .subject,
        "alice"
    );
}

#[tokio::test]
async fn auth_configuration_rejects_plaintext_and_ambiguous_audiences() {
    let issuer = TestIssuer::start().await;
    assert!(matches!(
        OidcProvider::discover(&issuer.issuer, false).await,
        Err(AuthError::InvalidConfig)
    ));
    assert!(matches!(
        Authenticator::discover(AuthConfig {
            issuer: issuer.issuer.clone(),
            user_audience: " ".into(),
            workload_audience: None,
            allow_loopback_http: true,
        })
        .await,
        Err(AuthError::InvalidConfig)
    ));
    assert!(matches!(
        Authenticator::discover(AuthConfig {
            issuer: issuer.issuer.clone(),
            user_audience: USER_AUDIENCE.into(),
            workload_audience: Some(USER_AUDIENCE.into()),
            allow_loopback_http: true,
        })
        .await,
        Err(AuthError::InvalidConfig)
    ));
    let unavailable = "http://127.0.0.1:1";
    assert!(matches!(
        OidcProvider::discover(unavailable, true).await,
        Err(AuthError::ProviderUnavailable)
    ));
}
