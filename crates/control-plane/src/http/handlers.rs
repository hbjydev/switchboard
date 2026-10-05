use super::{
    ApiState,
    dto::{
        AnswerRequest, AttemptRecordResponse, CancelRequest, CreateIssueRequest, DependencyRequest,
        EmptyRequest, EventResponse, IdentityResponse, IssueResponse, StatusResponse,
    },
    error::ApiError,
};
use crate::auth::{Principal, PrincipalKind};
use axum::{
    Extension, Json,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::StatusCode,
};
use ledger::IssueId;
use serde::Deserialize;
use uuid::Uuid;

type IdPath = Result<Path<Uuid>, PathRejection>;
type Body<T> = Result<Json<T>, JsonRejection>;
type IssueResult = Result<Json<IssueResponse>, ApiError>;

fn id(path: IdPath) -> Result<IssueId, ApiError> {
    path.map(|Path(id)| IssueId(id))
        .map_err(|_error| ApiError::invalid_request())
}

fn body<T>(body: Body<T>) -> Result<T, ApiError> {
    body.map(|Json(value)| value).map_err(|error| {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Request body exceeds the size limit",
            )
        } else {
            ApiError::invalid_request()
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListQuery {
    #[serde(default)]
    ready: bool,
}

pub(super) async fn list(
    State(state): State<ApiState>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Vec<IssueResponse>>, ApiError> {
    let Query(query) = query.map_err(|_error| ApiError::invalid_request())?;
    Ok(Json(
        state
            .application
            .list(query.ready)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

pub(super) async fn create(
    State(state): State<ApiState>,
    Extension(principal): Extension<Principal>,
    request: Body<CreateIssueRequest>,
) -> Result<(StatusCode, Json<IssueResponse>), ApiError> {
    let new = body(request)?.into_issue();
    let peer = authenticated_peer(&state, &principal).await?;
    Ok((
        StatusCode::CREATED,
        Json(state.application.create_as(peer.id, new).await?.into()),
    ))
}

pub(super) async fn get_issue(State(state): State<ApiState>, path: IdPath) -> IssueResult {
    Ok(Json(state.application.get(id(path)?).await?.into()))
}

pub(super) async fn events(
    State(state): State<ApiState>,
    path: IdPath,
) -> Result<Json<Vec<EventResponse>>, ApiError> {
    Ok(Json(
        state
            .application
            .events(id(path)?)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

pub(super) async fn attempts(
    State(state): State<ApiState>,
    path: IdPath,
) -> Result<Json<Vec<AttemptRecordResponse>>, ApiError> {
    Ok(Json(
        state
            .application
            .attempts(id(path)?)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

pub(super) async fn ready(
    State(state): State<ApiState>,
    Extension(principal): Extension<Principal>,
    path: IdPath,
    request: Body<EmptyRequest>,
) -> IssueResult {
    body(request)?;
    let peer = authenticated_peer(&state, &principal).await?;
    Ok(Json(
        state.application.ready_as(peer.id, id(path)?).await?.into(),
    ))
}

pub(super) async fn complete(
    State(state): State<ApiState>,
    Extension(principal): Extension<Principal>,
    path: IdPath,
    request: Body<EmptyRequest>,
) -> IssueResult {
    body(request)?;
    let peer = authenticated_peer(&state, &principal).await?;
    Ok(Json(
        state
            .application
            .complete_as(peer.id, id(path)?)
            .await?
            .into(),
    ))
}

pub(super) async fn cancel(
    State(state): State<ApiState>,
    Extension(principal): Extension<Principal>,
    path: IdPath,
    request: Body<CancelRequest>,
) -> IssueResult {
    let request = body(request)?;
    let peer = authenticated_peer(&state, &principal).await?;
    Ok(Json(
        state
            .application
            .cancel_as(peer.id, id(path)?, &request.reason)
            .await?
            .into(),
    ))
}

pub(super) async fn answer(
    State(state): State<ApiState>,
    Extension(principal): Extension<Principal>,
    path: IdPath,
    request: Body<AnswerRequest>,
) -> IssueResult {
    let request = body(request)?;
    let peer = authenticated_peer(&state, &principal).await?;
    Ok(Json(
        state
            .application
            .answer_as(peer.id, id(path)?, &request.answer)
            .await?
            .into(),
    ))
}

pub(super) async fn depend(
    State(state): State<ApiState>,
    Extension(principal): Extension<Principal>,
    path: IdPath,
    request: Body<DependencyRequest>,
) -> Result<StatusCode, ApiError> {
    let request = body(request)?;
    let peer = authenticated_peer(&state, &principal).await?;
    state
        .application
        .depend_as(peer.id, id(path)?, IssueId(request.dependency_id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn status(State(state): State<ApiState>) -> Json<StatusResponse> {
    Json(state.config.status.clone())
}

pub(super) async fn readiness(State(state): State<ApiState>) -> Result<StatusCode, ApiError> {
    state.application.check_ready().await?;
    state
        .config
        .auth
        .check_ready()
        .await
        .map_err(|_error| super::auth_unavailable())?;
    Ok(StatusCode::OK)
}

async fn authenticated_peer(
    state: &ApiState,
    principal: &Principal,
) -> Result<ledger::Peer, ApiError> {
    let kind = match principal.kind {
        PrincipalKind::User => ledger::PeerKind::Human,
        PrincipalKind::Workload => ledger::PeerKind::Agent,
    };
    Ok(state
        .application
        .identity(&principal.issuer, &principal.subject, kind)
        .await?)
}

pub(super) async fn identity(
    State(state): State<ApiState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<IdentityResponse>, ApiError> {
    let peer = authenticated_peer(&state, &principal).await?;
    Ok(Json(IdentityResponse {
        issuer: principal.issuer,
        subject: principal.subject,
        kind: match principal.kind {
            PrincipalKind::User => "User",
            PrincipalKind::Workload => "Workload",
        }
        .to_owned(),
        client_id: principal.client_id,
        peer_id: peer.id.0,
        scopes: principal.scopes.into_iter().collect(),
    }))
}
