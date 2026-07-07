//! Prometheus text-exposition parsing for the metrics the harness reports on.
//!
//! Only the handful of series the report needs are extracted; everything else
//! in `/metrics` is ignored. Parsing is line-oriented and tolerant: an absent
//! series reads as zero, matching a freshly started server.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

/// A point-in-time snapshot of the engine's counters and gauges.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MetricsSnapshot {
    /// `chronos_messages_processed_total`.
    pub messages: u64,
    /// `chronos_anomalies_detected_total{kind="prefix_hijack"}`.
    pub hijack: u64,
    /// `chronos_anomalies_detected_total{kind="path_leak"}`.
    pub leak: u64,
    /// `chronos_anomalies_detected_total{kind="route_churn"}`.
    pub churn: u64,
    /// `chronos_graph_nodes`.
    pub nodes: u64,
    /// `chronos_graph_edges`.
    pub edges: u64,
    /// `chronos_ingest_dropped_total`.
    pub dropped: u64,
    /// `chronos_deltas_broadcast_total`.
    pub deltas: u64,
    /// `chronos_connected_clients`.
    pub clients: u64,
}

impl MetricsSnapshot {
    /// Total anomalies across all kinds.
    pub fn anomalies(&self) -> u64 {
        self.hijack + self.leak + self.churn
    }

    /// Parse a Prometheus text exposition body.
    pub fn parse(body: &str) -> Self {
        let mut m = MetricsSnapshot::default();
        for line in body.lines() {
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = split_sample(line) else {
                continue;
            };
            // Metric values are floats on the wire (for example `1234` or
            // `1.2e3`); counters and gauges here are whole numbers, so truncate.
            let n = value.parse::<f64>().ok().map(|v| v as u64).unwrap_or(0);
            match key {
                "chronos_messages_processed_total" => m.messages = n,
                "chronos_graph_nodes" => m.nodes = n,
                "chronos_graph_edges" => m.edges = n,
                "chronos_ingest_dropped_total" => m.dropped = n,
                "chronos_deltas_broadcast_total" => m.deltas = n,
                "chronos_connected_clients" => m.clients = n,
                k if k.starts_with("chronos_anomalies_detected_total") => {
                    if k.contains("prefix_hijack") {
                        m.hijack = n;
                    } else if k.contains("path_leak") {
                        m.leak = n;
                    } else if k.contains("route_churn") {
                        m.churn = n;
                    }
                }
                _ => {}
            }
        }
        m
    }
}

/// Split a `metric_name{labels} value` sample line into its key (name plus
/// labels) and value token. Returns `None` for blank or malformed lines.
fn split_sample(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let idx = line.rfind(char::is_whitespace)?;
    let (key, value) = line.split_at(idx);
    Some((key.trim_end(), value.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_counters_and_labeled_series() {
        let body = "\
# HELP chronos_messages_processed_total total
# TYPE chronos_messages_processed_total counter
chronos_messages_processed_total 42
chronos_graph_nodes 1000
chronos_graph_edges 3000
chronos_ingest_dropped_total 0
chronos_deltas_broadcast_total 7
chronos_connected_clients 1
chronos_anomalies_detected_total{kind=\"prefix_hijack\"} 3
chronos_anomalies_detected_total{kind=\"path_leak\"} 2
chronos_anomalies_detected_total{kind=\"route_churn\"} 5
";
        let m = MetricsSnapshot::parse(body);
        assert_eq!(m.messages, 42);
        assert_eq!(m.nodes, 1000);
        assert_eq!(m.edges, 3000);
        assert_eq!(m.deltas, 7);
        assert_eq!(m.clients, 1);
        assert_eq!(m.hijack, 3);
        assert_eq!(m.leak, 2);
        assert_eq!(m.churn, 5);
        assert_eq!(m.anomalies(), 10);
    }

    #[test]
    fn absent_series_default_to_zero() {
        let m = MetricsSnapshot::parse("chronos_graph_nodes 5\n");
        assert_eq!(m.nodes, 5);
        assert_eq!(m.messages, 0);
        assert_eq!(m.anomalies(), 0);
    }

    #[test]
    fn tolerates_float_values() {
        let m = MetricsSnapshot::parse("chronos_messages_processed_total 1.0e3\n");
        assert_eq!(m.messages, 1000);
    }
}
