use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;

struct CallbackState {
    state: String,
    host: String,
    sender: Mutex<Option<oneshot::Sender<Result<String>>>>,
}

#[derive(Deserialize)]
struct CallbackQuery {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

pub(super) struct Callback {
    pub redirect_uri: String,
    code: oneshot::Receiver<Result<String>>,
    server: tokio::task::JoinHandle<()>,
}

impl Callback {
    pub async fn bind(port: u16, state: String) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        let host = listener.local_addr()?.to_string();
        let redirect_uri = format!("http://{host}/callback");
        let (sender, code) = oneshot::channel();
        let state = Arc::new(CallbackState {
            state,
            host,
            sender: Mutex::new(Some(sender)),
        });
        let router = axum::Router::new()
            .route(
                "/callback",
                get(
                    |State(state): State<Arc<CallbackState>>,
                     headers: HeaderMap,
                     Query(query): Query<CallbackQuery>| {
                        std::future::ready(receive(&state, &headers, query))
                    },
                ),
            )
            .with_state(state);
        let server = tokio::spawn(async move {
            if axum::serve(listener, router).await.is_err() {
                tracing::warn!("OIDC callback listener stopped");
            }
        });
        Ok(Self {
            redirect_uri,
            code,
            server,
        })
    }

    pub async fn code(&mut self) -> Result<String> {
        (&mut self.code)
            .await
            .map_err(|_closed| anyhow::anyhow!("OIDC callback listener stopped"))?
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn receive(
    state: &CallbackState,
    headers: &HeaderMap,
    query: CallbackQuery,
) -> (StatusCode, &'static str) {
    if headers.get("host").and_then(|host| host.to_str().ok()) != Some(state.host.as_str())
        || !bool::from(
            query
                .state
                .as_deref()
                .unwrap_or_default()
                .as_bytes()
                .ct_eq(state.state.as_bytes()),
        )
    {
        return (StatusCode::BAD_REQUEST, "Invalid login callback");
    }
    let result = match (query.code, query.error) {
        (Some(code), None) if !code.is_empty() => Ok(code),
        _ => Err(anyhow::anyhow!("the identity provider declined the login")),
    };
    let Ok(mut sender) = state.sender.lock() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Login callback unavailable",
        );
    };
    sender.take().map_or(
        (StatusCode::BAD_REQUEST, "Login callback already received"),
        |sender| {
            let _sent = sender.send(result);
            (
                StatusCode::OK,
                "Login response received. Return to Switchboard.",
            )
        },
    )
}
