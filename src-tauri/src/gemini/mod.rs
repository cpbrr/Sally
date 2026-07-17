//! Gemini clients: Live API WebSocket for realtime translation (design §4.2
//! item 4) and Developer API REST for optional cleanup (design §9).

pub mod cleanup;
pub mod live;

pub const LIVE_HOST: &str = "generativelanguage.googleapis.com";
pub const REST_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
