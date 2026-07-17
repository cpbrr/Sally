//! Gemini Live API client.
//!
//! Maintains the BidiGenerateContent WebSocket, streams 16 kHz mono PCM in,
//! and surfaces input (original) and output (translated) transcriptions.
//! Translated audio bytes from the model are discarded without being played
//! or saved (design §5 step 5). Reconnection policy lives in the session
//! orchestrator; this client reports a clean close reason instead.

use crate::config::redact_key;
use crate::error::{Result, SallyError};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone)]
pub enum LiveEvent {
    /// Setup acknowledged; audio may flow.
    Ready,
    /// Fragment of the original-language input transcription.
    Original(String),
    /// Fragment of the target-language output transcription.
    Translated(String),
    /// The model finished a turn; the assembler finalizes the entry.
    TurnComplete,
    /// Connection ended (reason, already key-redacted).
    Closed(String),
}

pub struct LiveConnection {
    pub audio_tx: mpsc::Sender<Vec<i16>>,
    pub events_rx: mpsc::Receiver<LiveEvent>,
}

/// Open one Live session. The caller owns retry/backoff.
pub async fn connect(
    api_key: &str,
    model: &str,
    target_language: &str,
) -> Result<LiveConnection> {
    let url = format!(
        "wss://{}/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={}",
        super::LIVE_HOST,
        api_key
    );
    let key = api_key.to_string();
    let redact = move |m: String| redact_key(&m, &key);

    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| SallyError::Gemini(redact(format!("live connect failed: {e}"))))?;
    let (mut sink, mut stream) = ws.split();

    // Setup message: request audio-out modality (required by live-translate
    // models) plus transcriptions of both directions. The audio itself is
    // discarded in the read loop.
    let setup = json!({
        "setup": {
            "model": format!("models/{model}"),
            "generationConfig": {
                "responseModalities": ["AUDIO"]
            },
            "systemInstruction": {
                "parts": [{
                    "text": format!(
                        "You are a live meeting translator. Detect the source language \
                         automatically and translate everything you hear into {target_language}. \
                         Translate faithfully without adding commentary."
                    )
                }]
            },
            "inputAudioTranscription": {},
            "outputAudioTranscription": {}
        }
    });
    sink.send(Message::Text(setup.to_string()))
        .await
        .map_err(|e| SallyError::Gemini(redact(format!("live setup failed: {e}"))))?;

    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<i16>>(64);
    let (events_tx, events_rx) = mpsc::channel::<LiveEvent>(256);

    // Writer: PCM chunks out as realtimeInput.
    tokio::spawn(async move {
        while let Some(samples) = audio_rx.recv().await {
            let mut bytes = Vec::with_capacity(samples.len() * 2);
            for s in samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let msg = json!({
                "realtimeInput": {
                    "audio": {
                        "mimeType": "audio/pcm;rate=16000",
                        "data": base64::engine::general_purpose::STANDARD.encode(&bytes)
                    }
                }
            });
            if sink.send(Message::Text(msg.to_string())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Reader: server messages in.
    let redact_reader = redact.clone();
    tokio::spawn(async move {
        while let Some(frame) = stream.next().await {
            let text = match frame {
                Ok(Message::Text(t)) => t.to_string(),
                Ok(Message::Binary(b)) => match String::from_utf8(b.to_vec()) {
                    Ok(t) => t,
                    Err(_) => continue,
                },
                Ok(Message::Close(reason)) => {
                    let why = reason
                        .map(|r| format!("{} {}", r.code, r.reason))
                        .unwrap_or_else(|| "connection closed".into());
                    let _ = events_tx.send(LiveEvent::Closed(redact_reader(why))).await;
                    return;
                }
                Ok(_) => continue,
                Err(e) => {
                    let _ = events_tx
                        .send(LiveEvent::Closed(redact_reader(format!("live error: {e}"))))
                        .await;
                    return;
                }
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            for event in parse_server_message(&value) {
                if events_tx.send(event).await.is_err() {
                    return;
                }
            }
        }
        let _ = events_tx
            .send(LiveEvent::Closed("stream ended".into()))
            .await;
    });

    Ok(LiveConnection {
        audio_tx,
        events_rx,
    })
}

/// Translate one server JSON message into client events. Model audio parts
/// (`inlineData`) are intentionally ignored — never played, never saved.
pub fn parse_server_message(value: &Value) -> Vec<LiveEvent> {
    let mut events = Vec::new();
    if value.get("setupComplete").is_some() {
        events.push(LiveEvent::Ready);
    }
    if let Some(content) = value.get("serverContent") {
        if let Some(text) = content
            .pointer("/inputTranscription/text")
            .and_then(Value::as_str)
        {
            if !text.is_empty() {
                events.push(LiveEvent::Original(text.to_string()));
            }
        }
        if let Some(text) = content
            .pointer("/outputTranscription/text")
            .and_then(Value::as_str)
        {
            if !text.is_empty() {
                events.push(LiveEvent::Translated(text.to_string()));
            }
        }
        if content
            .get("turnComplete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            events.push(LiveEvent::TurnComplete);
        }
    }
    if value.get("goAway").is_some() {
        events.push(LiveEvent::Closed("server requested reconnect".into()));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_transcriptions_and_turn_complete() {
        let msg = json!({
            "serverContent": {
                "inputTranscription": { "text": "こんにちは" },
                "outputTranscription": { "text": "xin chào" },
                "turnComplete": true
            }
        });
        let events = parse_server_message(&msg);
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], LiveEvent::Original(t) if t == "こんにちは"));
        assert!(matches!(&events[1], LiveEvent::Translated(t) if t == "xin chào"));
        assert!(matches!(events[2], LiveEvent::TurnComplete));
    }

    #[test]
    fn ignores_model_audio_parts() {
        let msg = json!({
            "serverContent": {
                "modelTurn": {
                    "parts": [{ "inlineData": { "mimeType": "audio/pcm", "data": "AAAA" } }]
                }
            }
        });
        assert!(parse_server_message(&msg).is_empty());
    }

    #[test]
    fn setup_complete_is_ready() {
        let events = parse_server_message(&json!({ "setupComplete": {} }));
        assert!(matches!(events.as_slice(), [LiveEvent::Ready]));
    }

    #[test]
    fn go_away_closes() {
        let events = parse_server_message(&json!({ "goAway": { "timeLeft": "1s" } }));
        assert!(matches!(events.as_slice(), [LiveEvent::Closed(_)]));
    }
}
