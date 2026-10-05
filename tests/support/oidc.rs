//! Local issuer with real RSA signatures, used only in isolated tests.
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{Form, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use control_plane::auth::{AuthConfig, Authenticator};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const CLI_ID: &str = "switchboard-cli";
pub const USER_AUDIENCE: &str = "switchboard-users";
pub const WORKLOAD_AUDIENCE: &str = "switchboard-workloads";

#[derive(Clone)]
struct LoginGrant {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    nonce: String,
    scope: String,
}

struct IssuerState {
    issuer: String,
    signing_key: EncodingKey,
    omit_refresh_nonce: AtomicBool,
    omit_token_expiry: AtomicBool,
    jwks: Mutex<Value>,
    codes: Mutex<HashMap<String, LoginGrant>>,
    refresh: Mutex<HashMap<String, LoginGrant>>,
}

pub struct TestIssuer {
    pub issuer: String,
    state: Arc<IssuerState>,
    task: tokio::task::JoinHandle<()>,
}

impl TestIssuer {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("issuer listener");
        let issuer = format!("http://{}", listener.local_addr().expect("issuer address"));
        let key: Value =
            serde_json::from_str(include_str!("keys/oidc.jwk.json")).expect("fixture public key");
        let state = Arc::new(IssuerState {
            issuer: issuer.clone(),
            signing_key: EncodingKey::from_rsa_pem(include_bytes!("keys/oidc.pem"))
                .expect("fixture signing key"),
            omit_refresh_nonce: AtomicBool::new(false),
            omit_token_expiry: AtomicBool::new(false),
            jwks: Mutex::new(json!({"keys":[key]})),
            codes: Mutex::new(HashMap::new()),
            refresh: Mutex::new(HashMap::new()),
        });
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(metadata))
            .route("/jwks", get(jwks))
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("mock issuer server");
        });
        Self {
            issuer,
            state,
            task,
        }
    }

    pub async fn authenticator(&self) -> Authenticator {
        Authenticator::discover(AuthConfig {
            issuer: self.issuer.clone(),
            user_audience: USER_AUDIENCE.into(),
            workload_audience: Some(WORKLOAD_AUDIENCE.into()),
            allow_loopback_http: true,
        })
        .await
        .expect("fixture authenticator")
    }

    #[must_use]
    pub fn user_claims(&self, subject: &str, scope: &str) -> Value {
        claims(&self.issuer, subject, USER_AUDIENCE, scope, CLI_ID)
    }

    #[must_use]
    pub fn user_token(&self, subject: &str, scope: &str) -> String {
        self.sign(&self.user_claims(subject, scope))
    }

    #[must_use]
    pub fn workload_claims(&self, client_id: &str, scope: &str) -> Value {
        claims(&self.issuer, client_id, WORKLOAD_AUDIENCE, scope, client_id)
    }

    #[must_use]
    pub fn workload_token(&self, client_id: &str, scope: &str) -> String {
        self.sign(&self.workload_claims(client_id, scope))
    }

    #[must_use]
    pub fn sign(&self, claims: &Value) -> String {
        self.sign_with_kid(claims, "fixture")
    }

    #[must_use]
    pub fn sign_with_kid(&self, claims: &Value, kid: &str) -> String {
        signed(claims, kid, &self.state.signing_key)
    }

    pub fn set_jwks(&self, jwks: Value) {
        *self.state.jwks.lock().expect("fixture keys lock") = jwks;
    }

    #[must_use]
    pub fn jwks(&self) -> Value {
        self.state.jwks.lock().expect("fixture keys lock").clone()
    }

    pub fn omit_nonce_on_refresh(&self) {
        self.state.omit_refresh_nonce.store(true, Ordering::SeqCst);
    }

    pub fn omit_expires_in(&self) {
        self.state.omit_token_expiry.store(true, Ordering::SeqCst);
    }
}

impl Drop for TestIssuer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
}

fn claims(issuer: &str, subject: &str, audience: &str, scope: &str, client_id: &str) -> Value {
    json!({"iss":issuer,"sub":subject,"aud":audience,"iat":now(),"nbf":now(),"exp":now()+3600,"scope":scope,"client_id":client_id})
}

fn signed(claims: &Value, kid: &str, key: &EncodingKey) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.to_owned());
    header.typ = Some("at+jwt".into());
    encode(&header, claims, key).expect("signed fixture JWT")
}

async fn metadata(State(state): State<Arc<IssuerState>>) -> Json<Value> {
    Json(json!({
        "issuer":state.issuer,
        "authorization_endpoint":format!("{}/authorize",state.issuer),
        "token_endpoint":format!("{}/token",state.issuer),
        "jwks_uri":format!("{}/jwks",state.issuer),
        "response_types_supported":["code"],
        "grant_types_supported":["authorization_code","refresh_token","client_credentials"],
        "code_challenge_methods_supported":["S256"],
        "id_token_signing_alg_values_supported":["RS256"],
        "token_endpoint_auth_methods_supported":["none","client_secret_basic","client_secret_post"]
    }))
}

async fn jwks(State(state): State<Arc<IssuerState>>) -> Json<Value> {
    Json(state.jwks.lock().expect("fixture keys lock").clone())
}

#[derive(Deserialize)]
struct AuthorizeRequest {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    scope: String,
    state: String,
    nonce: String,
    code_challenge: String,
    code_challenge_method: String,
}

async fn authorize(
    State(state): State<Arc<IssuerState>>,
    Query(request): Query<AuthorizeRequest>,
) -> Response {
    if request.response_type != "code"
        || request.client_id != CLI_ID
        || request.code_challenge_method != "S256"
    {
        return oauth_error("invalid_request");
    }
    let mut redirect = reqwest::Url::parse(&request.redirect_uri).expect("fixture callback URL");
    let code = Uuid::new_v4().to_string();
    state.codes.lock().expect("fixture code lock").insert(
        code.clone(),
        LoginGrant {
            client_id: request.client_id,
            redirect_uri: request.redirect_uri,
            challenge: request.code_challenge,
            nonce: request.nonce,
            scope: request.scope,
        },
    );
    redirect
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &request.state);
    Redirect::to(redirect.as_str()).into_response()
}

#[derive(Deserialize, Serialize)]
struct TokenRequest {
    grant_type: String,
    #[serde(default)]
    client_id: String,
    client_secret: Option<String>,
    code: Option<String>,
    code_verifier: Option<String>,
    redirect_uri: Option<String>,
    refresh_token: Option<String>,
    scope: Option<String>,
}

async fn token(
    State(state): State<Arc<IssuerState>>,
    headers: HeaderMap,
    Form(request): Form<TokenRequest>,
) -> Response {
    match request.grant_type.as_str() {
        "authorization_code" => code_token(&state, &request),
        "refresh_token" => refresh_token(&state, &request),
        "client_credentials" => workload_token(&state, &headers, &request),
        _ => oauth_error("unsupported_grant_type"),
    }
}

fn code_token(state: &IssuerState, request: &TokenRequest) -> Response {
    let code = request.code.as_deref().unwrap_or_default();
    let grant = state.codes.lock().expect("fixture code lock").remove(code);
    let Some(grant) = grant else {
        return oauth_error("invalid_grant");
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(
        request
            .code_verifier
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    ));
    if grant.client_id != request.client_id
        || request.redirect_uri.as_deref() != Some(&grant.redirect_uri)
        || challenge != grant.challenge
    {
        return oauth_error("invalid_grant");
    }
    user_tokens(state, grant, true)
}

fn refresh_token(state: &IssuerState, request: &TokenRequest) -> Response {
    let old = request.refresh_token.as_deref().unwrap_or_default();
    let grant = state
        .refresh
        .lock()
        .expect("fixture refresh lock")
        .remove(old);
    let Some(grant) = grant else {
        return oauth_error("invalid_grant");
    };
    if grant.client_id != request.client_id {
        return oauth_error("invalid_client");
    }
    user_tokens(
        state,
        grant,
        !state.omit_refresh_nonce.load(Ordering::SeqCst),
    )
}

fn user_tokens(state: &IssuerState, grant: LoginGrant, with_nonce: bool) -> Response {
    let access = signed(
        &claims(
            &state.issuer,
            "fixture-user",
            USER_AUDIENCE,
            &grant.scope,
            &grant.client_id,
        ),
        "fixture",
        &state.signing_key,
    );
    let mut id_claims = claims(
        &state.issuer,
        "fixture-user",
        &grant.client_id,
        "openid",
        &grant.client_id,
    );
    let fields = id_claims.as_object_mut().expect("fixture identity object");
    if with_nonce {
        fields.insert("nonce".into(), json!(grant.nonce));
    }
    fields.insert("azp".into(), json!(grant.client_id));
    let hash = Sha256::digest(access.as_bytes());
    fields.insert(
        "at_hash".into(),
        json!(URL_SAFE_NO_PAD.encode(hash.get(..16).expect("half of SHA256"))),
    );
    let id_token = signed(&id_claims, "fixture", &state.signing_key);
    let refresh = Uuid::new_v4().to_string();
    let scope = grant.scope.clone();
    state
        .refresh
        .lock()
        .expect("fixture refresh lock")
        .insert(refresh.clone(), grant);
    token_response(
        state,
        json!({"access_token":access,"id_token":id_token,"refresh_token":refresh,"token_type":"Bearer","scope":scope}),
    )
}

fn workload_token(state: &IssuerState, headers: &HeaderMap, request: &TokenRequest) -> Response {
    let basic = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|value| STANDARD.decode(value).ok())
        .and_then(|value| String::from_utf8(value).ok());
    let (client, secret) = basic
        .as_deref()
        .and_then(|value| value.split_once(':'))
        .unwrap_or_else(|| {
            (
                &request.client_id,
                request.client_secret.as_deref().unwrap_or_default(),
            )
        });
    if client != "fixture-workload" || secret != "fixture-secret" {
        return oauth_error("invalid_client");
    }
    let scope = request.scope.as_deref().unwrap_or("switchboard:read");
    token_response(
        state,
        json!({"access_token":signed(&claims(&state.issuer, client, WORKLOAD_AUDIENCE, scope, client),"fixture", &state.signing_key),"token_type":"Bearer","scope":scope}),
    )
}

fn token_response(state: &IssuerState, mut value: Value) -> Response {
    if !state.omit_token_expiry.load(Ordering::SeqCst) {
        value
            .as_object_mut()
            .expect("token response object")
            .insert("expires_in".into(), json!(3600));
    }
    Json(value).into_response()
}

fn oauth_error(error: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response()
}
