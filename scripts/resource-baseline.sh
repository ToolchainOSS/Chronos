#!/usr/bin/env bash
# Resource baseline harness for a typical Chronos instance.
#
# Runs the release chronos-server binary against the real RIPE RIS Live feed
# with geo resolution enabled (GeoLite2 downloaded) and the CAIDA dataset
# auto-downloaded, then samples the process's resident memory, CPU time, and
# the RIS TCP socket's received-byte counter over a measurement window.
#
# The goal is a reproducible, documented baseline: see
# docs/agents/resource-baseline.md. This is a measurement tool, not a test or a
# CI gate; it needs outbound network access to RIS, CAIDA, and the GeoLite2
# mirror.
#
# Usage:
#   scripts/resource-baseline.sh [warmup_secs] [window_secs]
# Defaults: warmup 45s, window 180s.
#
# Style note: comments avoid em dashes; they use colons, semicolons, and
# parentheses instead.
set -euo pipefail

WARMUP="${1:-45}"
WINDOW="${2:-180}"
SAMPLE_INTERVAL=5

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${CHRONOS_BIN:-$REPO_ROOT/target/release/chronos-server}"
BIND_ADDR="127.0.0.1:8087"
CLK_TCK="$(getconf CLK_TCK)"
NPROC="$(nproc)"

CITY_URL="${CHRONOS_GEOLITE2_CITY_URL:-https://s.joefang.org/GeoLite2-City}"
ASN_URL="${CHRONOS_GEOLITE2_ASN_URL:-https://s.joefang.org/GeoLite2-ASN}"

if [[ ! -x "$BIN" ]]; then
  echo "error: release binary not found at $BIN (run: cargo build --release --bin chronos-server)" >&2
  exit 1
fi

DATA_DIR="$(mktemp -d /tmp/chronos-baseline.XXXXXX)"
SERVER_PID=""
cleanup() {
  [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null || true
  [[ -n "$SERVER_PID" ]] && wait "$SERVER_PID" 2>/dev/null || true
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

echo "== Chronos resource baseline =="
echo "binary:        $BIN"
echo "host:          $NPROC vCPU, CLK_TCK=$CLK_TCK"
echo "data dir:      $DATA_DIR"
echo "warmup/window: ${WARMUP}s / ${WINDOW}s"
echo

echo "-- downloading GeoLite2 databases (typical instance has geo enabled) --"
curl -fsSL -o "$DATA_DIR/GeoLite2-City.mmdb" "$CITY_URL"
curl -fsSL -o "$DATA_DIR/GeoLite2-ASN.mmdb" "$ASN_URL"

echo "-- starting chronos-server against the real RIS Live feed --"
CHRONOS_BIND_ADDR="$BIND_ADDR" \
CHRONOS_DATA_DIR="$DATA_DIR" \
CHRONOS_GEOLITE2_CITY_DB="$DATA_DIR/GeoLite2-City.mmdb" \
CHRONOS_GEOLITE2_ASN_DB="$DATA_DIR/GeoLite2-ASN.mmdb" \
RUST_LOG=warn \
"$BIN" >"$DATA_DIR/server.log" 2>&1 &
SERVER_PID=$!

echo "-- waiting for readiness (pid $SERVER_PID) --"
for _ in $(seq 1 60); do
  if curl -fsS "http://$BIND_ADDR/readyz" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "error: server exited during startup; log:" >&2
    cat "$DATA_DIR/server.log" >&2
    exit 1
  fi
  sleep 1
done

# Helpers reading the kernel's per-process accounting.
rss_kb()   { awk '/^VmRSS:/ {print $2}' "/proc/$SERVER_PID/status"; }
cpu_ticks() {
  # utime (14) + stime (15); skip the comm field, which may contain spaces.
  awk '{ n=split($0,a," "); rest=substr($0, index($0,")")+2);
         m=split(rest,b," "); print b[12]+b[13] }' "/proc/$SERVER_PID/stat"
}
# Cumulative bytes received on the RIS Live TCP socket (peer port 80), summed
# across the server's established connections. We read the socket counter (not
# /proc/<pid>/io rchar, which does NOT count socket recv() traffic) so this
# reflects real wire ingress including WebSocket and TCP framing.
sock_rx_bytes() {
  ss -tinp state established '( dport = :80 )' 2>/dev/null | awk \
    -v tag="pid=$SERVER_PID," '
      index($0, tag) > 0 { mine = 1; next }
      mine && match($0, /bytes_received:[0-9]+/) {
        total += substr($0, RSTART + 15, RLENGTH - 15); mine = 0
      }
      END { print total + 0 }'
}
metric() { curl -fsS "http://$BIND_ADDR/metrics" 2>/dev/null | awk -v k="$1" '$1==k {print $2}'; }

echo "-- warmup ${WARMUP}s (CAIDA download, RIS connect, initial burst) --"
sleep "$WARMUP"

echo "-- measurement window ${WINDOW}s --"
start_ticks="$(cpu_ticks)"
start_rx="$(sock_rx_bytes)"
start_msgs="$(metric chronos_messages_processed_total)"; start_msgs="${start_msgs%.*}"
start_time="$(date +%s.%N)"

rss_sum=0; rss_peak=0; rss_n=0
samples=$(( WINDOW / SAMPLE_INTERVAL ))
for _ in $(seq 1 "$samples"); do
  sleep "$SAMPLE_INTERVAL"
  kill -0 "$SERVER_PID" 2>/dev/null || { echo "error: server died mid-window" >&2; cat "$DATA_DIR/server.log" >&2; exit 1; }
  r="$(rss_kb)"
  rss_sum=$(( rss_sum + r ))
  rss_n=$(( rss_n + 1 ))
  (( r > rss_peak )) && rss_peak="$r"
done

end_ticks="$(cpu_ticks)"
end_rx="$(sock_rx_bytes)"
end_msgs="$(metric chronos_messages_processed_total)"; end_msgs="${end_msgs%.*}"
nodes="$(metric chronos_graph_nodes)"; nodes="${nodes%.*}"
edges="$(metric chronos_graph_edges)"; edges="${edges%.*}"
dropped="$(metric chronos_ingest_dropped_total)"; dropped="${dropped%.*}"
end_time="$(date +%s.%N)"

# Compute the derived figures.
awk -v st="$start_ticks" -v et="$end_ticks" -v clk="$CLK_TCK" -v np="$NPROC" \
    -v sr="${start_rx:-0}" -v er="${end_rx:-0}" \
    -v sm="${start_msgs:-0}" -v em="${end_msgs:-0}" \
    -v t0="$start_time" -v t1="$end_time" \
    -v rss_sum="$rss_sum" -v rss_n="$rss_n" -v rss_peak="$rss_peak" \
    -v nodes="${nodes:-0}" -v edges="${edges:-0}" -v dropped="${dropped:-0}" '
BEGIN {
  dur = t1 - t0;
  cpu_secs = (et - st) / clk;
  cpu_pct_core = cpu_secs / dur * 100;
  cpu_pct_host = cpu_pct_core / np;
  # If the RIS socket reconnected mid-window its byte counter resets, making the
  # delta negative; fall back to the post-reconnect total and flag it.
  reconnected = 0;
  bytes = er - sr;
  if (bytes < 0) { bytes = er; reconnected = 1; }
  bps = bytes / dur;
  msgs = em - sm;
  mps = msgs / dur;
  rss_avg_mb = (rss_sum / rss_n) / 1024;
  rss_peak_mb = rss_peak / 1024;
  printf "\n== Baseline result (window %.0fs) ==\n", dur;
  printf "CPU:        %.2f%% of one core  (%.2f%% of %d-vCPU host)\n", cpu_pct_core, cpu_pct_host, np;
  printf "Memory RSS: %.1f MiB avg, %.1f MiB peak\n", rss_avg_mb, rss_peak_mb;
  printf "Ingress:    %.1f KiB/s  (%.2f GiB/day, %.1f GiB/month)%s\n", bps/1024, bps*86400/1073741824, bps*2592000/1073741824, (reconnected ? "  [socket reconnected: ingress approximate]" : "");
  printf "Throughput: %.0f RIS messages/s  (%.0f total in window)\n", mps, msgs;
  if (msgs > 0) printf "Per message: %.0f bytes ingress/message\n", bytes/msgs;
  printf "Topology:   %d ASNs, %d edges at window end\n", nodes, edges;
  printf "Dropped:    %d frames (bounded-channel backpressure)\n", dropped;
}'

echo
echo "-- server warnings (if any) --"
grep -iE 'warn|error' "$DATA_DIR/server.log" | head -20 || true
