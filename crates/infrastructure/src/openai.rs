//! Stateless, text-only `OpenAI` Responses adapter. Publication stays in application.
use async_trait::async_trait;
use reqwest::{
    Client, StatusCode, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use switchboard_agent::model::{GenerationRequest, GenerationResponse, LanguageModel, ModelError};
use switchboard_kernel::message::MessageContent;
use tokio_util::sync::CancellationToken;

/// Validated configuration. Deliberately has no Debug implementation: it holds a secret.
pub struct OpenAiConfig {
    authorization: HeaderValue,
    endpoint: Url,
    timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OpenAiConfigError {
    #[error("OpenAI API key must be nonblank and valid as an HTTP header")]
    InvalidApiKey,
    #[error("OpenAI timeout must be greater than zero")]
    InvalidTimeout,
    #[error(
        "OpenAI base URL must be HTTPS (or loopback HTTP), without credentials, query, or fragment"
    )]
    InvalidBaseUrl,
    #[error("could not initialize the OpenAI HTTP client")]
    Client,
}

impl OpenAiConfig {
    pub fn new(api_key: &str, timeout: Duration) -> Result<Self, OpenAiConfigError> {
        if api_key.trim().is_empty() {
            return Err(OpenAiConfigError::InvalidApiKey);
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_error| OpenAiConfigError::InvalidApiKey)?;
        authorization.set_sensitive(true);
        if timeout.is_zero() {
            return Err(OpenAiConfigError::InvalidTimeout);
        }
        let endpoint = Url::parse("https://api.openai.com/v1/responses")
            .map_err(|_error| OpenAiConfigError::InvalidBaseUrl)?;
        Ok(Self {
            authorization,
            endpoint,
            timeout,
        })
    }

    /// A base URL includes the API prefix, e.g. `https://api.openai.com/v1`.
    pub fn with_base_url(mut self, base_url: &str) -> Result<Self, OpenAiConfigError> {
        let mut url = Url::parse(base_url).map_err(|_error| OpenAiConfigError::InvalidBaseUrl)?;
        let loopback = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host == "[::1]"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(OpenAiConfigError::InvalidBaseUrl);
        }
        let path = format!("{}/responses", url.path().trim_end_matches('/'));
        url.set_path(&path);
        self.endpoint = url;
        Ok(self)
    }
}

pub struct OpenAiModel {
    client: Client,
    config: OpenAiConfig,
    cancellation: CancellationToken,
}

impl OpenAiModel {
    pub fn new(config: OpenAiConfig) -> Result<Self, OpenAiConfigError> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|_error| OpenAiConfigError::Client)?;
        Ok(Self {
            client,
            config,
            cancellation: CancellationToken::new(),
        })
    }

    /// Cancelling this token stops all current/future generations on this adapter.
    /// Construct a new adapter for a new lifecycle. Dropping a generation future
    /// also stops local waiting; neither action guarantees remote computation stops.
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    async fn send(&self, request: &GenerationRequest) -> Result<GenerationResponse, ModelError> {
        let body = project(request)?;
        let response = self
            .client
            .post(self.config.endpoint.clone())
            .header(AUTHORIZATION, self.config.authorization.clone())
            .json(&body)
            .send()
            .await
            .map_err(|error| transport_error(&error))?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        let response = response.json::<Response>().await.map_err(|error| {
            // Body-read timeouts may also be marked as decode errors by reqwest.
            if error.is_timeout() {
                ModelError::Timeout
            } else if error.is_decode() {
                ModelError::InvalidResponse
            } else {
                transport_error(&error)
            }
        })?;
        response.into_generation()
    }
}

#[async_trait]
impl LanguageModel for OpenAiModel {
    async fn generate(&self, request: GenerationRequest) -> Result<GenerationResponse, ModelError> {
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => Err(ModelError::Cancelled),
            result = tokio::time::timeout(self.config.timeout, self.send(&request)) => {
                result.map_err(|_error| ModelError::Timeout)?
            }
        }
    }
}

fn transport_error(error: &reqwest::Error) -> ModelError {
    if error.is_timeout() {
        ModelError::Timeout
    } else {
        ModelError::Unavailable
    }
}

fn status_error(status: StatusCode) -> ModelError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ModelError::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => ModelError::RateLimited,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => ModelError::Timeout,
        status if status.is_server_error() => ModelError::Unavailable,
        _ => ModelError::InvalidRequest,
    }
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    instructions: &'a str,
    input: Vec<InputMessage>,
    store: bool,
    stream: bool,
}

#[derive(Serialize)]
struct InputMessage {
    role: &'static str,
    content: String,
}

fn project(request: &GenerationRequest) -> Result<Request<'_>, ModelError> {
    if request.model.provider != "openai"
        || request.model.model.trim().is_empty()
        || request.messages.is_empty()
        || request
            .messages
            .iter()
            .any(|message| message.is_acting_agent != (message.speaker == request.acting_peer))
    {
        return Err(ModelError::InvalidRequest);
    }
    // Participant names and message text are always input data, never instructions.
    let participants = request
        .participants
        .iter()
        .map(|participant| {
            serde_json::json!({
                "peer_id": participant.peer_id.to_string(),
                "display_name": participant.display_name,
                "kind": format!("{:?}", participant.kind),
                "role": format!("{:?}", participant.role),
            })
        })
        .collect::<Vec<_>>();
    let mut input = vec![InputMessage {
        role: "user",
        content: serde_json::json!({
            "acting_peer_id": request.acting_peer.to_string(),
            "participants": participants,
        })
        .to_string(),
    }];
    input.extend(request.messages.iter().map(|message| {
        let MessageContent::Text(text) = &message.content;
        InputMessage {
            role: if message.is_acting_agent {
                "assistant"
            } else {
                "user"
            },
            content: serde_json::json!({
                "message_id": message.message_id.to_string(),
                "speaker_peer_id": message.speaker.to_string(),
                "display_name": message.display_name,
                "text": text,
            })
            .to_string(),
        }
    }));
    Ok(Request {
        model: &request.model.model,
        instructions: &request.instructions,
        input,
        store: false,
        stream: false,
    })
}

#[derive(Deserialize)]
struct Response {
    status: String,
    output: Vec<Output>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Output {
    Message {
        role: String,
        content: Vec<Content>,
    },
    Reasoning,
    #[serde(other)]
    Unsupported,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Content {
    OutputText {
        text: String,
    },
    Refusal,
    #[serde(other)]
    Unsupported,
}

impl Response {
    fn into_generation(self) -> Result<GenerationResponse, ModelError> {
        if self.status != "completed" {
            return Err(ModelError::InvalidResponse);
        }
        let mut text = String::new();
        for item in self.output {
            match item {
                Output::Reasoning => (),
                Output::Message { role, content } if role == "assistant" => {
                    for part in content {
                        match part {
                            Content::OutputText { text: part } => text.push_str(&part),
                            Content::Refusal => return Err(ModelError::Refused),
                            Content::Unsupported => return Err(ModelError::InvalidResponse),
                        }
                    }
                }
                _ => return Err(ModelError::InvalidResponse),
            }
        }
        if text.trim().is_empty() {
            return Err(ModelError::InvalidResponse);
        }
        Ok(GenerationResponse { text })
    }
}
