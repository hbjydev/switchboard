//! Deterministic local model with recorded requests and optional queued outcomes.
use async_trait::async_trait;
use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};
use switchboard_agent::model::{GenerationRequest, GenerationResponse, LanguageModel, ModelError};
use switchboard_kernel::message::MessageContent;

#[derive(Default)]
struct State {
    requests: Vec<GenerationRequest>,
    outcomes: VecDeque<Result<GenerationResponse, ModelError>>,
}

#[derive(Default)]
pub struct FakeModel {
    state: Mutex<State>,
}

impl FakeModel {
    fn lock(&self) -> Result<MutexGuard<'_, State>, ModelError> {
        self.state
            .lock()
            .map_err(|_poisoned| ModelError::Unavailable)
    }

    /// Returns snapshots, including requests whose generation failed.
    pub fn requests(&self) -> Result<Vec<GenerationRequest>, ModelError> {
        Ok(self.lock()?.requests.clone())
    }

    /// Overrides the next generation outcome, allowing explicit failure/retry tests.
    pub fn enqueue(
        &self,
        outcome: Result<GenerationResponse, ModelError>,
    ) -> Result<(), ModelError> {
        self.lock()?.outcomes.push_back(outcome);
        Ok(())
    }
}

#[async_trait]
impl LanguageModel for FakeModel {
    async fn generate(&self, request: GenerationRequest) -> Result<GenerationResponse, ModelError> {
        let text = request.messages.last().map_or("", |message| {
            let MessageContent::Text(text) = &message.content;
            text.as_str()
        });
        let default = GenerationResponse {
            text: format!("Fake reply from {}: {text}", request.acting_peer),
        };
        let mut state = self.lock()?;
        state.requests.push(request);
        let outcome = state.outcomes.pop_front();
        drop(state);
        outcome.unwrap_or(Ok(default))
    }
}
