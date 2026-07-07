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

/// Server-side RIS Live subscription filters.
///
/// Every field narrows the stream at the source, so RIS never sends frames the
/// engine would discard: this is how an operator trades global coverage for
/// lower bandwidth and higher signal. Fields borrow from the ingest config to
/// keep this type allocation-free on the (rare) subscribe path.
#[derive(Debug, Default, Clone, Copy)]
pub struct SubscribeFilters<'a> {
    /// Only messages from this collector (for example `rrc00`).
    pub host: Option<&'a str>,
    /// Only messages whose AS path matches this expression; a bare ASN matches
    /// any path traversing that AS ("anything involving my network").
    pub path: Option<&'a str>,
    /// Only updates covering this prefix.
    pub prefix: Option<&'a str>,
    /// With `prefix`, also include more-specific prefixes (catches sub-prefix
    /// hijacks). Ignored when `prefix` is unset.
    pub more_specific: bool,
    /// With `prefix`, also include less-specific prefixes. Ignored when `prefix`
    /// is unset.
    pub less_specific: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<&'a str>,
    #[serde(rename = "moreSpecific", skip_serializing_if = "is_false")]
    more_specific: bool,
    #[serde(rename = "lessSpecific", skip_serializing_if = "is_false")]
    less_specific: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Build the RIS Live subscription message.
///
/// The subscription targets UPDATE messages; withdrawals are delivered inside
/// UPDATE frames (RIS Live does not expose a separate WITHDRAW subscription).
/// The `moreSpecific`/`lessSpecific` flags only apply alongside a prefix, so
/// they are suppressed when no prefix filter is set.
pub fn subscribe_message(filters: &SubscribeFilters) -> String {
    let has_prefix = filters.prefix.is_some();
    let request = SubscribeRequest {
        kind: "ris_subscribe",
        data: SubscribeData {
            msg_type: "UPDATE",
            host: filters.host,
            path: filters.path,
            prefix: filters.prefix,
            more_specific: has_prefix && filters.more_specific,
            less_specific: has_prefix && filters.less_specific,
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
        let msg = subscribe_message(&SubscribeFilters::default());
        assert!(msg.contains("ris_subscribe"));
        assert!(msg.contains("UPDATE"));
        assert!(!msg.contains("host"));
        assert!(!msg.contains("path"));
        assert!(!msg.contains("prefix"));
        assert!(!msg.contains("moreSpecific"));
    }

    #[test]
    fn subscribe_message_includes_host_when_set() {
        let msg = subscribe_message(&SubscribeFilters {
            host: Some("rrc00"),
            ..Default::default()
        });
        assert!(msg.contains("rrc00"));
    }

    #[test]
    fn subscribe_message_includes_path_when_set() {
        let msg = subscribe_message(&SubscribeFilters {
            path: Some("64500"),
            ..Default::default()
        });
        assert!(msg.contains("\"path\":\"64500\""));
    }

    #[test]
    fn subscribe_message_includes_prefix_with_more_specific() {
        let msg = subscribe_message(&SubscribeFilters {
            prefix: Some("192.0.2.0/24"),
            more_specific: true,
            ..Default::default()
        });
        assert!(msg.contains("\"prefix\":\"192.0.2.0/24\""));
        assert!(msg.contains("\"moreSpecific\":true"));
    }

    #[test]
    fn subscribe_message_suppresses_specificity_without_prefix() {
        // moreSpecific/lessSpecific are meaningless without a prefix, so they
        // must not appear in the subscription when no prefix is set.
        let msg = subscribe_message(&SubscribeFilters {
            more_specific: true,
            less_specific: true,
            ..Default::default()
        });
        assert!(!msg.contains("moreSpecific"));
        assert!(!msg.contains("lessSpecific"));
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
