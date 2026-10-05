use super::dto::{ErrorDetail, ErrorResponse};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub(super) struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn invalid_request() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Invalid request parameters or JSON body",
        )
    }
}

impl From<ledger::Error> for ApiError {
    fn from(error: ledger::Error) -> Self {
        let (status, code, message) = match error {
            ledger::Error::NotFound(id) => (
                StatusCode::NOT_FOUND,
                "issue_not_found",
                format!("Issue {id} not found"),
            ),
            ledger::Error::InvalidTransition(_) => (
                StatusCode::CONFLICT,
                "invalid_transition",
                "Issue cannot perform this operation in its current state".into(),
            ),
            ledger::Error::IneligibleActor => (
                StatusCode::FORBIDDEN,
                "ineligible_actor",
                "Actor is not eligible for this operation".into(),
            ),
            ledger::Error::DependencyCycle => (
                StatusCode::CONFLICT,
                "dependency_cycle",
                "Dependency would create a cycle".into(),
            ),
            ledger::Error::EmptyField(field) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "empty_field",
                format!("{field} must not be blank"),
            ),
            ledger::Error::PeerKindConflict => (
                StatusCode::CONFLICT,
                "peer_kind_conflict",
                "Peer name already belongs to a different kind".into(),
            ),
            ledger::Error::IdentityKindConflict => (
                StatusCode::FORBIDDEN,
                "identity_kind_conflict",
                "Authenticated identity is bound to a different principal kind".into(),
            ),
            ledger::Error::ExecutionLost(_) => (
                StatusCode::CONFLICT,
                "execution_lost",
                "Execution attempt has lost authority".into(),
            ),
            ledger::Error::Database(_) | ledger::Error::SchemaNotReady => (
                StatusCode::SERVICE_UNAVAILABLE,
                "ledger_unavailable",
                "Ledger is unavailable or schema is incompatible".into(),
            ),
            ledger::Error::Migration(_) | ledger::Error::InvalidLeaseDuration => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Internal control-plane error".into(),
            ),
        };
        // Never format database errors, internal chains, or credentials at the boundary.
        Self::new(status, code, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: ErrorDetail {
                    code: self.code.to_owned(),
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}
