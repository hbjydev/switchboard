//! Versioned HTTP boundary, authentication, and safe request tracing.
pub mod dto;
mod error;
mod handlers;

use axum::{
    Router,
    extract::{DefaultBodyLimit, MatchedPath, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::{sync::Arc, time::Instant};
use tracing::Instrument;
use uuid::Uuid;

use crate::{
    Application,
    auth::{AuthError, Authenticator, Principal, PrincipalKind},
};
pub use dto::StatusResponse;
use error::ApiError;

/// Verified identity and scope checks are mandatory for the versioned API.
pub struct ApiConfig {
    pub auth: Authenticator,
    pub status: StatusResponse,
}

#[derive(Clone)]
struct ApiState {
    application: Application,
    config: Arc<ApiConfig>,
}

pub fn router(application: Application, config: ApiConfig) -> Router {
    let state = ApiState {
        application,
        config: Arc::new(config),
    };
    let api = Router::new()
        .route("/me", get(handlers::identity))
        .route("/status", get(handlers::status))
        .route("/issues", get(handlers::list).post(handlers::create))
        .route("/issues/{id}", get(handlers::get_issue))
        .route("/issues/{id}/events", get(handlers::events))
        .route("/issues/{id}/attempts", get(handlers::attempts))
        .route("/issues/{id}/ready", post(handlers::ready))
        .route("/issues/{id}/complete", post(handlers::complete))
        .route("/issues/{id}/cancel", post(handlers::cancel))
        .route("/issues/{id}/answer", post(handlers::answer))
        .route("/issues/{id}/dependencies", post(handlers::depend))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .nest("/api/v1", api)
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/readyz", get(handlers::readiness))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(trace_request))
        .with_state(state)
}

async fn authenticate(State(state): State<ApiState>, mut request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(token) = token else {
        return unauthorized();
    };
    let principal = match state.config.auth.authenticate(token).await {
        Ok(principal) => principal,
        Err(AuthError::ProviderUnavailable) => return auth_unavailable().into_response(),
        Err(_) => return unauthorized(),
    };
    if let Err(error) = authorize(&principal, request.method()) {
        return error.into_response();
    }
    request.extensions_mut().insert(principal);
    next.run(request).await
}

fn authorize(principal: &Principal, method: &Method) -> Result<(), ApiError> {
    let read = *method == Method::GET || *method == Method::HEAD;
    if !read && principal.kind != PrincipalKind::User {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "workload_not_authorized",
            "Workload identities cannot perform human issue operations",
        ));
    }
    let scope = if read {
        "switchboard:read"
    } else {
        "switchboard:write"
    };
    if !principal.has_scope(scope) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "insufficient_scope",
            "Access token lacks the required scope",
        ));
    }
    Ok(())
}

fn unauthorized() -> Response {
    let mut response = ApiError::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "A valid issuer-signed access token is required",
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn auth_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "authentication_unavailable",
        "Authentication provider keys are unavailable",
    )
}

async fn trace_request(request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    let span = tracing::info_span!("http_request", method = %request.method(), route, request_id);
    let start = Instant::now();
    let mut response = async {
        let response = next.run(request).await;
        tracing::info!(
            status = response.status().as_u16(),
            latency_ms = start.elapsed().as_millis(),
            "request completed"
        );
        response
    }
    .instrument(span)
    .await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

async fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "route_not_found",
        "API route not found",
    )
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "Method not allowed for this route",
    )
}
