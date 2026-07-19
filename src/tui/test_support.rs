// src/tui/test_support.rs
//
// Shared test doubles for the TUI test modules — one home instead of each
// `#[cfg(test)]` module hand-copying its own near-identical mock model (the
// workspace tests' `EchoModel` was a drifted copy of `app`'s
// `StreamingEchoModel` before this module existed).

use daimon::model::types::{ChatRequest, ChatResponse, Message, StopReason, Usage};
use daimon::stream::{ResponseStream, StreamEvent};

/// Replies with a two-token streamed response and no tool calls.
///
/// Deliberately emits no `StreamEvent::Usage` of its own: `Agent::prompt_stream`
/// (see `daimon`'s `agent/runner.rs`) always appends its *own* estimated
/// `Usage` event per ReAct iteration (character-count-based, `chars/4`) after
/// forwarding whatever the model's stream yields — so any `Usage` this mock
/// emitted would be forwarded too and summed with the agent's, on top of the
/// agent's own estimate. Omitting it keeps usage assertions tied to one
/// authoritative source instead of an arbitrary double-count.
pub(crate) struct StreamingEchoModel;

impl daimon::model::Model for StreamingEchoModel {
    async fn generate(&self, _request: &ChatRequest) -> daimon::Result<ChatResponse> {
        Ok(ChatResponse {
            message: Message::assistant("unused"),
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage::default()),
        })
    }
    async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta("Hello".into())),
            Ok(StreamEvent::TextDelta(", world".into())),
            Ok(StreamEvent::Done),
        ])))
    }
}

/// A model whose stream is fed by the test through a channel, so a test can
/// hold a turn open across ticks — e.g. switch windows mid-stream and assert
/// a hidden window's transcript keeps advancing, or that the tab bar shows
/// the `✻` busy marker for a background window.
///
/// The stream may only be opened once per instance (one turn per test).
pub(crate) struct ChannelModel {
    events:
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<daimon::Result<StreamEvent>>>>,
}

impl ChannelModel {
    pub(crate) fn new() -> (
        Self,
        tokio::sync::mpsc::UnboundedSender<daimon::Result<StreamEvent>>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            ChannelModel {
                events: std::sync::Mutex::new(Some(rx)),
            },
            tx,
        )
    }
}

impl daimon::model::Model for ChannelModel {
    async fn generate(&self, _request: &ChatRequest) -> daimon::Result<ChatResponse> {
        Ok(ChatResponse {
            message: Message::assistant("unused"),
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage::default()),
        })
    }
    async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
        let rx = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("ChannelModel's stream may only be opened once");
        // The stream must END after `Done` (not merely yield it): daimon's
        // runner polls the model stream to exhaustion, so a channel-backed
        // stream that stays open would leave the turn awaiting forever even
        // though the test already sent `Done`.
        Ok(Box::pin(futures::stream::unfold(
            (rx, false),
            |(mut rx, finished)| async move {
                if finished {
                    return None;
                }
                let event = rx.recv().await?;
                let is_done = matches!(event, Ok(StreamEvent::Done));
                Some((event, (rx, is_done)))
            },
        )))
    }
}
