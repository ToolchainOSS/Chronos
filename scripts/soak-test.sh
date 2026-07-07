#!/usr/bin/env bash
# Chronos production soak test.
#
# Runs the release chronos-server binary against the REAL RIPE RIS Live feed for
# an extended window to prove the engine survives sustained production load, and
# produces a self-contained Markdown report suitable for attaching to a CI run
# (GitHub Actions step summary + artifact). Three signals are captured, matching
# what an operator needs to trust a deployment:
#
#   1. Console log:   the server's own INFO / WARN / ERROR stream (RUST_LOG=info),
#                     summarized (level counts, reconnects, first ERROR/WARN
#                     lines) with the full log kept as an artifact.
#   2. Resource use:  a SEPARATE monitor process samples the server's kernel
#                     accounting (RSS, CPU ticks, RIS socket bytes) plus the
#                     Prometheus counters every few seconds into a CSV time
#                     series, so memory growth and CPU are visible over time.
#   3. Performance:   throughput (RIS msg/s), per-message wire cost, ingest drop
#                     ratio, anomalies detected by kind, topology growth, and the
#                     WebSocket egress path exercised end to end by a probe.
#
# This is a measurement/assurance tool, not a unit test or a PR gate. It needs
# outbound network to RIS, CAIDA, and the GeoLite2 mirror. Missing optional data
# (GeoLite2) degrades gracefully and is reported, never fatal.
#
# Usage:
#   scripts/soak-test.sh [duration_secs] [warmup_secs] [sample_interval_secs]
# Defaults: duration 1200s (20 min), warmup 60s, sample interval 10s.
#
# Key env overrides:
#   CHRONOS_BIN            release binary (default target/release/chronos-server)
#   CHRONOS_WS_PROBE_BIN   ws_probe example (default <bindir>/examples/ws_probe)
#   CHRONOS_RIS_HOST       optional RIS collector filter (cuts ingress)
#   CHRONOS_RIS_URL        override the RIS Live URL
#   SOAK_OUT_DIR           output directory (default a fresh mktemp dir)
#   SOAK_SKIP_GEO          set to 1 to skip the GeoLite2 download
#   CHRONOS_GEOLITE2_CITY_URL / CHRONOS_GEOLITE2_ASN_URL   GeoLite2 mirrors
#
# Exit code: 0 on PASS or WARN, 2 on FAIL (server crash/panic). See the Verdict
# section of the report.
#
# Style note: comments avoid em dashes; they use colons, semicolons, and
# parentheses instead.
set -euo pipefail

DURATION="${1:-1200}"
WARMUP="${2:-60}"
SAMPLE_INTERVAL="${3:-10}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${CHRONOS_BIN:-$REPO_ROOT/target/release/chronos-server}"
WS_PROBE_BIN="${CHRONOS_WS_PROBE_BIN:-$(dirname "$BIN")/examples/ws_probe}"
BIND_ADDR="127.0.0.1:8089"
METRICS_URL="http://$BIND_ADDR/metrics"
READY_URL="http://$BIND_ADDR/readyz"
WS_URL="ws://$BIND_ADDR/ws"
CLK_TCK="$(getconf CLK_TCK)"
NPROC="$(nproc)"

CITY_URL="${CHRONOS_GEOLITE2_CITY_URL:-https://s.joefang.org/GeoLite2-City}"
ASN_URL="${CHRONOS_GEOLITE2_ASN_URL:-https://s.joefang.org/GeoLite2-ASN}"

if [[ ! -x "$BIN" ]]; then
  echo "error: release binary not found at $BIN (run: cargo build --release --bin chronos-server)" >&2
  exit 1
fi

OUT_DIR="${SOAK_OUT_DIR:-$(mktemp -d /tmp/chronos-soak.XXXXXX)}"
mkdir -p "$OUT_DIR"
DATA_DIR="$OUT_DIR/data"
mkdir -p "$DATA_DIR"
LOG="$OUT_DIR/server.log"
CSV="$OUT_DIR/samples.csv"
REPORT="${SOAK_REPORT:-$OUT_DIR/soak-report.md}"
WS_OUT="$OUT_DIR/ws_probe.txt"
STATUS="$OUT_DIR/monitor.status"
: >"$STATUS"

SERVER_PID=""
cleanup() {
  [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null || true
  [[ -n "$SERVER_PID" ]] && wait "$SERVER_PID" 2>/dev/null || true
}
trap cleanup EXIT

echo "== Chronos soak test =="
echo "binary:        $BIN"
echo "host:          $NPROC vCPU, CLK_TCK=$CLK_TCK"
echo "out dir:       $OUT_DIR"
echo "duration:      warmup ${WARMUP}s + window ${DURATION}s"
echo "RIS host:      ${CHRONOS_RIS_HOST:-<unfiltered firehose>}"
echo

# --- Kernel accounting helpers (per-process), matching resource-baseline.sh. ---
rss_kb() { awk '/^VmRSS:/ {print $2}' "/proc/$SERVER_PID/status" 2>/dev/null || echo 0; }
cpu_ticks() {
  # utime + stime from /proc/<pid>/stat; skip the comm field (may contain spaces).
  awk '{ rest=substr($0, index($0,")")+2); m=split(rest,b," "); print b[12]+b[13] }' \
    "/proc/$SERVER_PID/stat" 2>/dev/null || echo 0
}
# Cumulative bytes received on the RIS Live TCP socket (peer port 80). Reads the
# socket counter directly because /proc/<pid>/io rchar does NOT count socket recv.
sock_rx_bytes() {
  ss -tinp state established '( dport = :80 )' 2>/dev/null | awk \
    -v tag="pid=$SERVER_PID," '
      index($0, tag) > 0 { mine = 1; next }
      mine && match($0, /bytes_received:[0-9]+/) {
        total += substr($0, RSTART + 15, RLENGTH - 15); mine = 0
      }
      END { print total + 0 }'
}
# Parse the Prometheus text exposition into a single CSV fragment:
# msgs,hijack,leak,churn,nodes,edges,dropped,deltas,clients
parse_metrics() {
  awk '
    $1=="chronos_messages_processed_total"       {msgs=$2}
    /^chronos_anomalies_detected_total\{kind="prefix_hijack"\}/ {hij=$2}
    /^chronos_anomalies_detected_total\{kind="path_leak"\}/     {leak=$2}
    /^chronos_anomalies_detected_total\{kind="route_churn"\}/   {churn=$2}
    $1=="chronos_graph_nodes"                    {nodes=$2}
    $1=="chronos_graph_edges"                    {edges=$2}
    $1=="chronos_ingest_dropped_total"           {dropped=$2}
    $1=="chronos_deltas_broadcast_total"         {deltas=$2}
    $1=="chronos_connected_clients"              {clients=$2}
    END { printf "%d,%d,%d,%d,%d,%d,%d,%d,%d\n",
          msgs+0, hij+0, leak+0, churn+0, nodes+0, edges+0, dropped+0, deltas+0, clients+0 }'
}

# --- Optional GeoLite2 download (a typical instance has geo enabled). ---
GEO_ARGS=()
GEO_STATUS="disabled (download skipped)"
if [[ "${SOAK_SKIP_GEO:-0}" != "1" ]]; then
  echo "-- downloading GeoLite2 databases (geo enabled path) --"
  if curl -fsSL -o "$DATA_DIR/GeoLite2-City.mmdb" "$CITY_URL" \
     && curl -fsSL -o "$DATA_DIR/GeoLite2-ASN.mmdb" "$ASN_URL"; then
    GEO_ARGS=(
      "CHRONOS_GEOLITE2_CITY_DB=$DATA_DIR/GeoLite2-City.mmdb"
      "CHRONOS_GEOLITE2_ASN_DB=$DATA_DIR/GeoLite2-ASN.mmdb"
    )
    GEO_STATUS="enabled (GeoLite2 City + ASN downloaded)"
  else
    GEO_STATUS="disabled (GeoLite2 download failed; graceful degradation exercised)"
    echo "warn: GeoLite2 download failed; continuing without geo (AreaDegraded disabled)" >&2
  fi
fi

# --- Start the server with full INFO logging captured to a file. ---
echo "-- starting chronos-server against the real RIS Live feed --"
env \
  CHRONOS_BIND_ADDR="$BIND_ADDR" \
  CHRONOS_DATA_DIR="$DATA_DIR" \
  RUST_LOG="${RUST_LOG:-info,chronos_ingest=info,chronos_server=info}" \
  "${GEO_ARGS[@]}" \
  "$BIN" >"$LOG" 2>&1 &
SERVER_PID=$!

echo "-- waiting for readiness (pid $SERVER_PID) --"
for _ in $(seq 1 60); do
  if curl -fsS "$READY_URL" >/dev/null 2>&1; then break; fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "error: server exited during startup; log:" >&2
    cat "$LOG" >&2
    exit 1
  fi
  sleep 1
done

echo "-- warmup ${WARMUP}s (CAIDA download, RIS connect, initial topology burst) --"
sleep "$WARMUP"

# --- Separate monitor process: samples resource + performance counters. ---
START_EPOCH="$(date +%s)"
echo "epoch,elapsed_s,rss_kb,cpu_ticks,sock_rx,msgs,hijack,leak,churn,nodes,edges,dropped,deltas,clients" >"$CSV"

monitor() {
  local samples=$1 interval=$2 start=$3 i now elapsed rss ticks rx frag
  for ((i = 0; i < samples; i++)); do
    sleep "$interval"
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "SERVER_DIED" >>"$STATUS"
      break
    fi
    now="$(date +%s)"
    elapsed=$((now - start))
    rss="$(rss_kb)"
    ticks="$(cpu_ticks)"
    rx="$(sock_rx_bytes)"
    frag="$(curl -fsS "$METRICS_URL" 2>/dev/null | parse_metrics)"
    [[ -z "$frag" ]] && frag="0,0,0,0,0,0,0,0,0"
    echo "$now,$elapsed,$rss,$ticks,$rx,$frag" >>"$CSV"
  done
}

SAMPLES=$((DURATION / SAMPLE_INTERVAL))
echo "-- measurement window ${DURATION}s ($SAMPLES samples @ ${SAMPLE_INTERVAL}s, separate monitor process) --"
monitor "$SAMPLES" "$SAMPLE_INTERVAL" "$START_EPOCH" &
MON_PID=$!

# --- Exercise the WebSocket egress path end to end (bonus assurance). ---
# A subscriber makes deltas_broadcast / connected_clients meaningful (the server
# skips the broadcast when nobody is listening) and proves the full pipeline.
WS_STATUS="not run (probe binary absent)"
if [[ -x "$WS_PROBE_BIN" ]]; then
  probe_secs=$((DURATION < 300 ? DURATION : 300))
  echo "-- WS egress probe for ${probe_secs}s --"
  WS_PROBE_SECS="$probe_secs" "$WS_PROBE_BIN" "$WS_URL" >"$WS_OUT" 2>&1 &
  WS_STATUS="running"
fi

wait "$MON_PID" 2>/dev/null || true

# --- Snapshot final state, then stop the server. ---
SERVER_ALIVE=1
kill -0 "$SERVER_PID" 2>/dev/null || SERVER_ALIVE=0
END_EPOCH="$(date +%s)"

if [[ -f "$WS_OUT" ]]; then
  WS_STATUS="$(cat "$WS_OUT")"
fi

# --- Aggregate the CSV time series into headline figures. ---
if [[ "$(wc -l <"$CSV")" -lt 3 ]]; then
  echo "error: too few samples collected; server may have died early. Log tail:" >&2
  tail -30 "$LOG" >&2
fi

# awk emits shell-assignable VAR=value lines (pre-formatted, no spaces).
metrics_env="$(awk -F, -v clk="$CLK_TCK" -v np="$NPROC" '
  NR==1 { next }                       # header
  {
    if (first==0) { first=1; f_ep=$1; f_ticks=$4; f_rx=$5; f_msgs=$6 }
    l_ep=$1; l_ticks=$4; l_rx=$5; l_msgs=$6;
    l_hij=$7; l_leak=$8; l_churn=$9; l_nodes=$10; l_edges=$11; l_dropped=$12; l_deltas=$13; l_clients=$14;
    rss_sum += $3; rss_n++;
    if ($3 > rss_peak) rss_peak = $3;
    if ($14 > clients_peak) clients_peak = $14;
  }
  END {
    if (rss_n == 0) { print "SAMPLE_COUNT=0"; exit }
    dur = l_ep - f_ep; if (dur <= 0) dur = 1;
    cpu_secs = (l_ticks - f_ticks) / clk;
    cpu_core = cpu_secs / dur * 100;
    reconnected = 0; bytes = l_rx - f_rx;
    if (bytes < 0) { bytes = l_rx; reconnected = 1; }
    bps = bytes / dur;
    msgs = l_msgs - f_msgs; mps = msgs / dur;
    anom = l_hij + l_leak + l_churn;
    denom = msgs + l_dropped; drop_ratio = (denom > 0) ? (l_dropped / denom * 100) : 0;
    printf "SAMPLE_COUNT=%d\n", rss_n;
    printf "DUR_S=%d\n", dur;
    printf "CPU_CORE_PCT=%.1f\n", cpu_core;
    printf "CPU_HOST_PCT=%.1f\n", cpu_core / np;
    printf "RSS_AVG_MB=%.1f\n", (rss_sum / rss_n) / 1024;
    printf "RSS_PEAK_MB=%.1f\n", rss_peak / 1024;
    printf "ING_KIBS=%.1f\n", bps / 1024;
    printf "ING_GIB_DAY=%.2f\n", bps * 86400 / 1073741824;
    printf "ING_RECONNECTED=%d\n", reconnected;
    printf "MSGS_TOTAL=%d\n", msgs;
    printf "MSGS_PER_S=%.0f\n", mps;
    printf "BYTES_PER_MSG=%s\n", (msgs > 0 ? sprintf("%.0f", bytes / msgs) : "n/a");
    printf "DROPPED=%d\n", l_dropped;
    printf "DROP_RATIO_PCT=%.3f\n", drop_ratio;
    printf "ANOM_TOTAL=%d\n", anom;
    printf "ANOM_HIJACK=%d\n", l_hij;
    printf "ANOM_LEAK=%d\n", l_leak;
    printf "ANOM_CHURN=%d\n", l_churn;
    printf "NODES=%d\n", l_nodes;
    printf "EDGES=%d\n", l_edges;
    printf "DELTAS=%d\n", l_deltas;
    printf "CLIENTS_PEAK=%d\n", clients_peak;
  }' "$CSV")"
# Defaults so the report renders even if aggregation found nothing.
SAMPLE_COUNT=0; DUR_S=0; CPU_CORE_PCT=0; CPU_HOST_PCT=0; RSS_AVG_MB=0; RSS_PEAK_MB=0
ING_KIBS=0; ING_GIB_DAY=0; ING_RECONNECTED=0; MSGS_TOTAL=0; MSGS_PER_S=0; BYTES_PER_MSG=n/a
DROPPED=0; DROP_RATIO_PCT=0; ANOM_TOTAL=0; ANOM_HIJACK=0; ANOM_LEAK=0; ANOM_CHURN=0
NODES=0; EDGES=0; DELTAS=0; CLIENTS_PEAK=0
eval "$metrics_env"

# --- Log analysis: level counts, reconnects, panics, provider selection. ---
level_count() { awk -v want="$1" '$2==want {n++} END{print n+0}' "$LOG"; }
INFO_N="$(level_count INFO)"
WARN_N="$(level_count WARN)"
ERROR_N="$(level_count ERROR)"
PANIC_N="$(grep -c 'panicked at' "$LOG" || true)"
RECONNECTS="$(grep -c 'ingest: subscribed to UPDATE stream' "$LOG" || true)"

if grep -q 'loaded CAIDA AS relationships' "$LOG"; then
  REL_PROVIDER="CAIDA AS-relationship dataset"
else
  REL_PROVIDER="degree-based heuristic (CAIDA unavailable)"
fi

# --- Verdict. FAIL only on a genuine crash/panic; a dead upstream feed is a ---
# --- WARN (external, transient), matching the repo's non-blocking live-data ---
# --- philosophy. ---
VERDICT="PASS"
VERDICT_NOTES=""
add_note() { VERDICT_NOTES+="- $1"$'\n'; }
if [[ "$SERVER_ALIVE" -eq 0 ]] || grep -q 'SERVER_DIED' "$STATUS"; then
  VERDICT="FAIL"; add_note "Server process did not survive the window (crash)."
fi
if [[ "$PANIC_N" -gt 0 ]]; then
  VERDICT="FAIL"; add_note "Detected $PANIC_N panic(s) in the log."
fi
if [[ "$MSGS_TOTAL" -eq 0 && "$VERDICT" != "FAIL" ]]; then
  VERDICT="WARN"; add_note "Zero RIS messages processed in the window (upstream feed unreachable or empty)."
fi
if [[ "$ERROR_N" -gt 0 && "$VERDICT" == "PASS" ]]; then
  VERDICT="WARN"; add_note "$ERROR_N ERROR-level log line(s); review the log."
fi
# Drop ratio above 1% suggests the bounded ingest channel could not keep up.
if awk -v r="$DROP_RATIO_PCT" 'BEGIN{exit !(r > 1.0)}'; then
  [[ "$VERDICT" == "PASS" ]] && VERDICT="WARN"
  add_note "Ingest drop ratio ${DROP_RATIO_PCT}% exceeds 1% (backpressure); review sizing."
fi
[[ -z "$VERDICT_NOTES" ]] && VERDICT_NOTES="- No anomalies in behavior; all checks nominal."$'\n'

VERDICT_ICON="✅"
[[ "$VERDICT" == "WARN" ]] && VERDICT_ICON="⚠️"
[[ "$VERDICT" == "FAIL" ]] && VERDICT_ICON="❌"

# --- Render the Markdown report. ---
{
  echo "# Chronos production soak report"
  echo
  echo "## $VERDICT_ICON Verdict: $VERDICT"
  echo
  echo "$VERDICT_NOTES"
  echo "## Run"
  echo
  echo "| Field | Value |"
  echo "|---|---|"
  echo "| Date (UTC) | $(date -u '+%Y-%m-%d %H:%M:%S') |"
  echo "| Window | ${DUR_S}s measured (${WARMUP}s warmup, ${SAMPLE_INTERVAL}s sampling, $SAMPLE_COUNT samples) |"
  echo "| Host | $NPROC vCPU, CLK_TCK=$CLK_TCK |"
  echo "| RIS feed | ${CHRONOS_RIS_HOST:+collector filter: $CHRONOS_RIS_HOST}${CHRONOS_RIS_HOST:-unfiltered firehose} |"
  echo "| Geo | $GEO_STATUS |"
  echo "| Relationships | $REL_PROVIDER |"
  echo "| RIS (re)connections | $RECONNECTS |"
  echo
  echo "## Resource usage"
  echo
  echo "| Resource | Value |"
  echo "|---|---|"
  echo "| CPU | ${CPU_CORE_PCT}% of one core (${CPU_HOST_PCT}% of the ${NPROC}-vCPU host) |"
  echo "| Memory RSS | ${RSS_AVG_MB} MiB avg, ${RSS_PEAK_MB} MiB peak |"
  if [[ "$ING_RECONNECTED" -eq 1 ]]; then
    echo "| Ingress | ${ING_KIBS} KiB/s (~${ING_GIB_DAY} GiB/day) [socket reconnected: approximate] |"
  else
    echo "| Ingress | ${ING_KIBS} KiB/s (~${ING_GIB_DAY} GiB/day) |"
  fi
  echo
  echo "## Performance"
  echo
  echo "| Metric | Value |"
  echo "|---|---|"
  echo "| Throughput | ${MSGS_PER_S} RIS msg/s (${MSGS_TOTAL} in window) |"
  echo "| Per message | ${BYTES_PER_MSG} bytes ingress/message |"
  echo "| Ingest dropped | ${DROPPED} frames (${DROP_RATIO_PCT}% of received) |"
  echo "| Topology | ${NODES} ASNs, ${EDGES} edges at window end |"
  echo "| Anomalies | ${ANOM_TOTAL} total (hijack ${ANOM_HIJACK}, leak ${ANOM_LEAK}, churn ${ANOM_CHURN}) |"
  echo "| Deltas broadcast | ${DELTAS} (peak ${CLIENTS_PEAK} WS client(s)) |"
  echo "| WS egress probe | ${WS_STATUS#ws_probe: } |"
  echo
  echo "## Time series"
  echo
  echo "| t (s) | RSS MiB | msgs | Δmsg/s | ASNs | edges | dropped | clients |"
  echo "|--:|--:|--:|--:|--:|--:|--:|--:|"
  awk -F, '
    NR==1 { next }
    { rows[n]=$0; n++ }
    END {
      step = int(n / 12); if (step < 1) step = 1;
      prev_msgs=""; prev_t="";
      for (i = 0; i < n; i += step) {
        split(rows[i], c, ",");
        t=c[2]; rss=c[3]/1024; msgs=c[6]; nodes=c[10]; edges=c[11]; dropped=c[12]; clients=c[14];
        rate = (prev_msgs=="") ? 0 : (msgs - prev_msgs) / ((t - prev_t) > 0 ? (t - prev_t) : 1);
        printf "| %d | %.1f | %d | %.0f | %d | %d | %d | %d |\n", t, rss, msgs, rate, nodes, edges, dropped, clients;
        prev_msgs=msgs; prev_t=t;
      }
    }' "$CSV"
  echo
  echo "## Console log"
  echo
  echo "| Level | Count |"
  echo "|---|--:|"
  echo "| INFO | $INFO_N |"
  echo "| WARN | $WARN_N |"
  echo "| ERROR | $ERROR_N |"
  echo "| panic | $PANIC_N |"
  echo
  if [[ "$WARN_N" -gt 0 || "$ERROR_N" -gt 0 ]]; then
    echo "First WARN/ERROR lines:"
    echo
    echo '```text'
    awk '$2=="WARN" || $2=="ERROR"' "$LOG" | head -20
    echo '```'
    echo
  fi
  echo "<details><summary>Startup log (first 25 lines)</summary>"
  echo
  echo '```text'
  head -25 "$LOG"
  echo '```'
  echo
  echo "</details>"
} >"$REPORT"

echo
echo "== $VERDICT =="
echo "report:  $REPORT"
echo "log:     $LOG"
echo "samples: $CSV"
echo

if [[ "$VERDICT" == "FAIL" ]]; then
  exit 2
fi
