//! Frame parsing and subscription helpers.
//!
//! These functions are intentionally pure (no I/O) so that parser resilience can
//! be exercised by unit tests using mocked payloads.

use chronos_types::{RisData, RisEnvelope};
use serde::Serialize;

/// Error produced while decoding an incoming frame.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The frame was not valid JSON or did not match the RIS Live schema.
    #[error("failed to decode RIS frame: {0}")]
    Json(#[from] serde_json::Error),
}

/// The subscription request sent after the socket opens.
#[derive(Debug, Serialize)]
struct SubscribeRequest<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    data: SubscribeData<'a>,
}

#[derive(Debug, Serialize)]
struct SubscribeData<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<&'a str>,
}

/// Build the RIS Live subscription message.
///
/// The subscription targets UPDATE messages; withdrawals are delivered inside
/// UPDATE frames (RIS Live does not expose a separate WITHDRAW subscription).
pub fn subscribe_message(host: Option<&str>) -> String {
    let request = SubscribeRequest {
        kind: "ris_subscribe",
        data: SubscribeData {
            msg_type: "UPDATE",
            host,
        },
    };
    // Serialization of this small fixed structure cannot fail in practice.
    serde_json::to_string(&request)
        .unwrap_or_else(|_| r#"{"type":"ris_subscribe","data":{"type":"UPDATE"}}"#.to_string())
}

/// Parse a raw frame.
///
/// Returns `Ok(Some(data))` for routing messages (`ris_message`), `Ok(None)` for
/// control frames that carry no routing payload (for example `pong` or
/// `ris_error`), and `Err` when the frame is not decodable.
pub fn parse_message(bytes: &[u8]) -> Result<Option<RisData>, ParseError> {
    let envelope: RisEnvelope = serde_json::from_slice(bytes)?;
    if envelope.is_ris_message() {
        Ok(Some(envelope.data))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronos_types::RisMessageType;

    #[test]
    fn subscribe_message_targets_updates() {
        let msg = subscribe_message(None);
        assert!(msg.contains("ris_subscribe"));
        assert!(msg.contains("UPDATE"));
        assert!(!msg.contains("host"));
    }

    #[test]
    fn subscribe_message_includes_host_when_set() {
        let msg = subscribe_message(Some("rrc00"));
        assert!(msg.contains("rrc00"));
    }

    #[test]
    fn parses_routing_message() {
        let raw = br#"{"type":"ris_message","data":{"type":"UPDATE","path":[64500]}}"#;
        let parsed = parse_message(raw).unwrap().unwrap();
        assert_eq!(parsed.msg_type, RisMessageType::Update);
    }

    #[test]
    fn skips_control_frame() {
        let raw = br#"{"type":"pong","data":{}}"#;
        assert!(parse_message(raw).unwrap().is_none());
    }

    #[test]
    fn errors_on_truncated_frame() {
        let raw = br#"{"type":"ris_message","data":{"type":"#;
        assert!(parse_message(raw).is_err());
    }

    #[test]
    fn errors_on_non_json() {
        assert!(parse_message(b"this is not json").is_err());
    }

    #[test]
    fn errors_on_bad_prefix() {
        let raw = br#"{"type":"ris_message","data":{"type":"UPDATE",
            "withdrawals":["999.999.999.999/33"]}}"#;
        assert!(parse_message(raw).is_err());
    }
}
