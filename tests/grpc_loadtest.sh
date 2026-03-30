#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────
# Crap CMS — gRPC load tests with ghz
#
# Proper gRPC benchmarking with per-request latency percentiles
# (p50/p95/p99), error breakdowns, and histogram data.
#
# Companion to tests/loadtest.sh (which covers admin HTTP via oha
# and raw grpcurl concurrency). This script uses ghz for accurate
# gRPC latency measurement.
#
# Prerequisites:
#   ghz       — go install github.com/bojand/ghz/cmd/ghz@latest
#   grpcurl   — gRPC testing (for auth/setup)
#   protoc    — proto compiler (generates descriptor set for ghz)
#   jq        — JSON parsing
#   Running server: cargo run -- -C ./example serve
#
# Usage:
#   ./tests/grpc_loadtest.sh                           # defaults
#   ./tests/grpc_loadtest.sh --duration 5              # shorter runs
#   ./tests/grpc_loadtest.sh --concurrency 1,10        # custom levels
#   ./tests/grpc_loadtest.sh --scenarios find,count    # specific tests
# ──────────────────────────────────────────────────────────────

set -euo pipefail

# Force C locale for numeric formatting (avoid comma decimals)
export LC_NUMERIC=C

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROTO_DIR="${SCRIPT_DIR}/../proto"

# ── Configuration ────────────────────────────────────────────

GRPC_ADDR="${GRPC_ADDR:-localhost:50051}"
DURATION="${DURATION:-10}"
CONCURRENCY_LEVELS="1 10 50"
SCENARIOS=""
EMAIL="${EMAIL:-admin@crap.studio}"
PASSWORD="${PASSWORD:-admin123}"

# Parse args
while [[ $# -gt 0 ]]; do
    case $1 in
        --duration)    DURATION="$2"; shift 2 ;;
        --concurrency) CONCURRENCY_LEVELS="${2//,/ }"; shift 2 ;;
        --scenarios)   SCENARIOS="${2//,/ }"; shift 2 ;;
        --email)       EMAIL="$2"; shift 2 ;;
        --password)    PASSWORD="$2"; shift 2 ;;
        --help|-h)
            echo "Usage: $0 [--duration SEC] [--concurrency N,N,...] [--scenarios NAME,...] [--email E] [--password P]"
            echo ""
            echo "Scenarios: describe, count, find, find_where, find_by_id, find_deep, create, update"
            echo "Defaults:  duration=10, concurrency=1,10,50, all scenarios"
            exit 0
            ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

# Order: light reads first, heavy/write scenarios last (to avoid poisoning measurements)
ALL_SCENARIOS="describe count find find_where find_by_id find_deep create update"
if [[ -z "$SCENARIOS" ]]; then
    SCENARIOS="$ALL_SCENARIOS"
fi

# ── Colors & formatting ─────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

header() { echo -e "\n${BOLD}${BLUE}── $1 ──${RESET}"; }
info()   { echo -e "${DIM}$1${RESET}"; }
ok()     { echo -e "${GREEN}✓${RESET} $1"; }
warn()   { echo -e "${YELLOW}!${RESET} $1"; }
fail()   { echo -e "${RED}✗${RESET} $1"; }

# ── Results tracking ─────────────────────────────────────────

declare -a RESULT_LINES=()

add_result() {
    # add_result scenario concurrency rps p50 p95 p99 errors
    RESULT_LINES+=("$1|$2|$3|$4|$5|$6|$7")
}

print_summary() {
    header "Results Summary"
    printf "${BOLD}%-20s %5s %10s %10s %10s %10s %8s${RESET}\n" \
        "Scenario" "Conc" "Req/s" "p50" "p95" "p99" "Errors"
    printf "%-20s %5s %10s %10s %10s %10s %8s\n" \
        "────────────────────" "─────" "──────────" "──────────" "──────────" "──────────" "────────"
    for line in "${RESULT_LINES[@]}"; do
        IFS='|' read -r scenario conc rps p50 p95 p99 errors <<< "$line"
        local error_color="$RESET"
        if [[ "$errors" != "0" && "$errors" != "0.00%" && "$errors" != "-" ]]; then
            error_color="$RED"
        fi
        printf "%-20s %5s %10s %10s %10s %10s ${error_color}%8s${RESET}\n" \
            "$scenario" "$conc" "$rps" "$p50" "$p95" "$p99" "$errors"
    done
    echo ""
    info "Duration per test: ${DURATION}s | Concurrency levels: ${CONCURRENCY_LEVELS}"
    echo ""
    echo -e "${BOLD}What to look for:${RESET}"
    echo "  - Req/s drops sharply at high concurrency → pool exhaustion"
    echo "  - p99 spikes while p50 stays flat → lock contention"
    echo "  - Errors at concurrency 50 → connection pool timeout / deadlock"
    echo "  - gRPC writes much slower than reads → expected, but 10x+ = problem"
}

# ── Pre-flight checks ───────────────────────────────────────

header "Pre-flight checks"

MISSING=0
for cmd in ghz grpcurl protoc jq; do
    if command -v "$cmd" &>/dev/null; then
        ok "$cmd found: $(command -v "$cmd")"
    else
        fail "$cmd not found — install it first"
        MISSING=1
    fi
done

if [[ $MISSING -eq 1 ]]; then
    echo ""
    fail "Missing dependencies. Install with:"
    echo "  go install github.com/bojand/ghz/cmd/ghz@latest"
    echo "  (grpcurl: https://github.com/fullstorydev/grpcurl)"
    echo "  (protoc: sudo pacman -S protobuf)"
    echo "  (jq: sudo pacman -S jq)"
    exit 1
fi

# Compile proto descriptor set (ghz doesn't support reflection v1)
PROTOSET=$(mktemp /tmp/crap-loadtest-XXXXXX.protoset)
trap "rm -f '$PROTOSET'" EXIT

if protoc --descriptor_set_out="$PROTOSET" --include_imports \
    -I "$PROTO_DIR" "$PROTO_DIR/content.proto" 2>/dev/null; then
    ok "Proto descriptor set compiled"
else
    fail "Failed to compile proto descriptor set"
    echo "  Check that ${PROTO_DIR}/content.proto exists"
    exit 1
fi

# Check gRPC server is up
if grpcurl -plaintext "$GRPC_ADDR" list &>/dev/null; then
    ok "gRPC server responding at ${GRPC_ADDR}"
else
    fail "gRPC server not responding at ${GRPC_ADDR}"
    echo "  Start it with: cargo run -- -C ./example serve"
    exit 1
fi

# ── Auth setup ───────────────────────────────────────────────

header "Auth setup"

# Get JWT token via gRPC Login
LOGIN_RESP=$(grpcurl -plaintext -d "{
    \"collection\": \"users\",
    \"email\": \"${EMAIL}\",
    \"password\": \"${PASSWORD}\"
}" "$GRPC_ADDR" crap.ContentAPI/Login 2>&1) || {
    fail "gRPC Login failed. Create a user first:"
    echo "  cargo run -- -C ./example user create -e ${EMAIL} -p ${PASSWORD} -f role=admin -f name='Admin'"
    echo "  Response: ${LOGIN_RESP}"
    exit 1
}

JWT_TOKEN=$(echo "$LOGIN_RESP" | jq -r '.token // empty')
if [[ -z "$JWT_TOKEN" ]]; then
    fail "No token in Login response: ${LOGIN_RESP}"
    exit 1
fi
ok "Got JWT token (${#JWT_TOKEN} chars)"

# ── Grab real IDs for tests ──────────────────────────────────

POST_ID=$(grpcurl -plaintext -H "authorization: Bearer ${JWT_TOKEN}" -d '{
    "collection": "posts",
    "limit": "1"
}' "$GRPC_ADDR" crap.ContentAPI/Find 2>/dev/null | jq -r '.documents[0].id // empty')

if [[ -z "$POST_ID" ]]; then
    warn "No posts found in DB — find_by_id/update tests will be skipped"
    warn "Run the seed migration first to populate test data"
else
    ok "Using post ID: ${POST_ID}"
fi

# Get the logged-in user's ID for write tests (author field is required on posts)
USER_ID=$(grpcurl -plaintext -d "{\"token\": \"${JWT_TOKEN}\"}" \
    "$GRPC_ADDR" crap.ContentAPI/Me 2>/dev/null | jq -r '.user.id // empty')

if [[ -z "$USER_ID" ]]; then
    warn "Could not get user ID — create test will be skipped"
else
    ok "Using user ID: ${USER_ID}"
fi

# ── ghz test runner ──────────────────────────────────────────

# Format nanoseconds (integer) to human-readable latency
format_latency() {
    local val="$1"
    if [[ "$val" =~ ^[0-9.]+$ ]]; then
        awk "BEGIN { v = $val / 1000000; if (v < 1) printf \"%.2fms\", v; else if (v < 100) printf \"%.1fms\", v; else printf \"%.0fms\", v }"
    else
        echo "$val"
    fi
}

# Run ghz and parse JSON output for results
run_ghz() {
    local label="$1"
    local concurrency="$2"
    local rpc="$3"
    local data="$4"
    local auth="${5:-}"

    info "  ${label} @ c=${concurrency} for ${DURATION}s..."

    local cmd=(
        ghz --insecure
        --protoset "$PROTOSET"
        --call "crap.ContentAPI/${rpc}"
        --duration "${DURATION}s"
        --concurrency "$concurrency"
        --format json
    )

    if [[ -n "$auth" ]]; then
        cmd+=(--metadata "{\"authorization\":\"Bearer ${auth}\"}")
    fi

    cmd+=(-d "$data")
    cmd+=("$GRPC_ADDR")

    local output
    output=$("${cmd[@]}" 2>/dev/null) || true

    local rps p50 p95 p99 total total_err errors

    rps=$(echo "$output" | jq -r '.rps // 0' 2>/dev/null \
        | xargs printf "%.1f" 2>/dev/null || echo "?")

    p50=$(echo "$output" | jq -r '
        [.latencyDistribution[] | select(.percentage == 50) | .latency]
        | .[0] // "?"
    ' 2>/dev/null || echo "?")

    p95=$(echo "$output" | jq -r '
        [.latencyDistribution[] | select(.percentage == 95) | .latency]
        | .[0] // "?"
    ' 2>/dev/null || echo "?")

    p99=$(echo "$output" | jq -r '
        [.latencyDistribution[] | select(.percentage == 99) | .latency]
        | .[0] // "?"
    ' 2>/dev/null || echo "?")

    total=$(echo "$output" | jq -r '.count // 0' 2>/dev/null || echo "0")
    total_err=$(echo "$output" | jq -r '
        [.statusCodeDistribution // {} | to_entries[]
         | select(.key != "OK") | .value] | add // 0
    ' 2>/dev/null || echo "0")

    if [[ "$total" -gt 0 && "$total_err" -gt 0 ]]; then
        errors=$(awk "BEGIN { printf \"%.2f%%\", ($total_err / $total) * 100 }")
    else
        errors="0"
    fi

    p50=$(format_latency "$p50")
    p95=$(format_latency "$p95")
    p99=$(format_latency "$p99")

    add_result "$label" "$concurrency" "$rps" "$p50" "$p95" "$p99" "$errors"

    # Brief cooldown between runs so the server can drain in-flight requests
    # and release connection/thread pool resources. Without this, heavy
    # scenarios (find_deep c=50) poison all subsequent measurements.
    sleep 2
}

# ── Scenario runners ─────────────────────────────────────────

scenario_find() {
    header "Scenario: gRPC Find (depth=0, limit=10)"
    local data='{"collection":"posts","limit":"10","depth":"0"}'
    for c in $CONCURRENCY_LEVELS; do
        run_ghz "find" "$c" "Find" "$data" "$JWT_TOKEN"
    done
}

scenario_find_deep() {
    header "Scenario: gRPC Find deep (depth=2)"
    local data='{"collection":"posts","depth":"2"}'
    for c in $CONCURRENCY_LEVELS; do
        run_ghz "find_deep" "$c" "Find" "$data" "$JWT_TOKEN"
    done
}

scenario_find_by_id() {
    if [[ -z "${POST_ID:-}" ]]; then
        warn "Skipping find_by_id — no posts in DB"
        return
    fi
    header "Scenario: gRPC FindByID"
    local data="{\"collection\":\"posts\",\"id\":\"${POST_ID}\"}"
    for c in $CONCURRENCY_LEVELS; do
        run_ghz "find_by_id" "$c" "FindByID" "$data" "$JWT_TOKEN"
    done
}

scenario_find_where() {
    header "Scenario: gRPC Find with where clause"
    local data='{"collection":"posts","where":"{\"post_type\":{\"equals\":\"article\"}}","limit":"10"}'
    for c in $CONCURRENCY_LEVELS; do
        run_ghz "find_where" "$c" "Find" "$data" "$JWT_TOKEN"
    done
}

scenario_count() {
    header "Scenario: gRPC Count"
    local data='{"collection":"posts"}'
    for c in $CONCURRENCY_LEVELS; do
        run_ghz "count" "$c" "Count" "$data" "$JWT_TOKEN"
    done
}

scenario_create() {
    if [[ -z "${USER_ID:-}" ]]; then
        warn "Skipping create — no user ID available"
        return
    fi
    header "Scenario: gRPC Create"
    # ghz template: {{.UUID}} generates a unique slug per request
    local data="{\"collection\":\"posts\",\"data\":{\"title\":\"Loadtest ghz {{.UUID}}\",\"slug\":\"loadtest-ghz-{{.UUID}}\",\"post_type\":\"article\",\"author\":\"${USER_ID}\",\"content\":\"Load test document.\"}}"
    for c in $CONCURRENCY_LEVELS; do
        run_ghz "create" "$c" "Create" "$data" "$JWT_TOKEN"
    done

    # Cleanup: delete all loadtest posts
    info "  Cleaning up loadtest posts..."
    local deleted
    deleted=$(grpcurl -plaintext -H "authorization: Bearer ${JWT_TOKEN}" -d '{
        "collection": "posts",
        "where": "{\"slug\":{\"like\":\"loadtest-ghz-%\"}}"
    }' "$GRPC_ADDR" crap.ContentAPI/DeleteMany 2>/dev/null \
        | jq -r '.deleted // 0')
    ok "Cleaned up ${deleted} loadtest posts"
}

scenario_update() {
    if [[ -z "${POST_ID:-}" ]]; then
        warn "Skipping update — no posts in DB"
        return
    fi
    header "Scenario: gRPC Update"
    # Idempotent update — same field value each time
    local data="{\"collection\":\"posts\",\"id\":\"${POST_ID}\",\"data\":{\"content\":\"Updated by ghz loadtest.\"}}"
    for c in $CONCURRENCY_LEVELS; do
        run_ghz "update" "$c" "Update" "$data" "$JWT_TOKEN"
    done
}

scenario_describe() {
    header "Scenario: gRPC DescribeCollection"
    local data='{"slug":"posts"}'
    for c in $CONCURRENCY_LEVELS; do
        run_ghz "describe" "$c" "DescribeCollection" "$data" "$JWT_TOKEN"
    done
}

# ── Run scenarios ────────────────────────────────────────────

echo ""
info "Configuration: duration=${DURATION}s, concurrency=[${CONCURRENCY_LEVELS}]"
info "Scenarios: ${SCENARIOS}"

for scenario in $SCENARIOS; do
    case "$scenario" in
        find)         scenario_find ;;
        find_deep)    scenario_find_deep ;;
        find_by_id)   scenario_find_by_id ;;
        find_where)   scenario_find_where ;;
        count)        scenario_count ;;
        create)       scenario_create ;;
        update)       scenario_update ;;
        describe)     scenario_describe ;;
        *)            warn "Unknown scenario: $scenario" ;;
    esac
done

# ── Summary ──────────────────────────────────────────────────

print_summary
