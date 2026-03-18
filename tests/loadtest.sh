#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────
# Crap CMS — Local load/performance tests
#
# Finds concurrency bugs (deadlocks, pool exhaustion, Lua VM
# starvation), measures throughput, and establishes a baseline.
#
# Prerequisites:
#   oha       — pacman -S oha (Rust HTTP load tester)
#   grpcurl   — already installed (gRPC testing)
#   jq        — JSON parsing
#   Running server: cargo run -- -C ./example serve
#
# Usage:
#   ./tests/loadtest.sh                         # defaults
#   ./tests/loadtest.sh --duration 5            # shorter runs
#   ./tests/loadtest.sh --concurrency 1,10      # custom levels
#   ./tests/loadtest.sh --scenarios read_list,grpc_find  # specific tests
# ──────────────────────────────────────────────────────────────

set -euo pipefail

# ── Configuration ────────────────────────────────────────────

ADMIN_URL="${ADMIN_URL:-http://localhost:3000}"
GRPC_ADDR="${GRPC_ADDR:-localhost:50051}"
DURATION="${DURATION:-10}"
CONCURRENCY_LEVELS="1 10 50"
SCENARIOS=""
EMAIL="${EMAIL:-admin@example.com}"
PASSWORD="${PASSWORD:-secret123}"

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
            echo "Scenarios: read_list, read_single, grpc_find, grpc_find_deep, grpc_write, search"
            echo "Defaults:  duration=10, concurrency=1,10,50, all scenarios"
            exit 0
            ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

ALL_SCENARIOS="read_list read_single grpc_find grpc_find_deep grpc_write search"
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
for cmd in oha grpcurl jq curl; do
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
    echo "  sudo pacman -S oha jq curl"
    echo "  (grpcurl: https://github.com/fullstorydev/grpcurl)"
    exit 1
fi

# Check server is up
if curl -sf -o /dev/null --max-time 3 "${ADMIN_URL}/admin/login"; then
    ok "Admin server responding at ${ADMIN_URL}"
else
    fail "Admin server not responding at ${ADMIN_URL}"
    echo "  Start it with: cargo run -- -C ./example serve"
    exit 1
fi

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
    echo "  cargo run -- user create ./example -e ${EMAIL} -p ${PASSWORD} -f role=admin -f name='Admin'"
    echo "  Response: ${LOGIN_RESP}"
    exit 1
}

JWT_TOKEN=$(echo "$LOGIN_RESP" | jq -r '.token // empty')
if [[ -z "$JWT_TOKEN" ]]; then
    fail "No token in Login response: ${LOGIN_RESP}"
    exit 1
fi
ok "Got JWT token (${#JWT_TOKEN} chars)"

# Get admin session cookie via POST /admin/login
# Step 1: GET /admin/login to get CSRF cookie
CSRF_RESP=$(curl -sS -D - -o /dev/null --max-time 5 "${ADMIN_URL}/admin/login")
CSRF_TOKEN=$(echo "$CSRF_RESP" | grep -oP 'crap_csrf=\K[^;]+' | head -1)

if [[ -z "$CSRF_TOKEN" ]]; then
    fail "Could not get CSRF token from GET /admin/login"
    exit 1
fi

# Step 2: POST /admin/login with CSRF + credentials
LOGIN_HEADERS=$(curl -sS -D - -o /dev/null --max-time 5 \
    -b "crap_csrf=${CSRF_TOKEN}" \
    -d "collection=users&email=${EMAIL}&password=${PASSWORD}&_csrf=${CSRF_TOKEN}" \
    "${ADMIN_URL}/admin/login")

SESSION_COOKIE=$(echo "$LOGIN_HEADERS" | grep -oP 'crap_session=\K[^;]+' | head -1)

if [[ -z "$SESSION_COOKIE" ]]; then
    fail "Could not get session cookie from admin login"
    info "Response headers:"
    echo "$LOGIN_HEADERS" | head -20
    exit 1
fi
ok "Got admin session cookie (${#SESSION_COOKIE} chars)"

COOKIE_HEADER="crap_session=${SESSION_COOKIE}; crap_csrf=${CSRF_TOKEN}"

# ── Grab real IDs for tests ──────────────────────────────────

POST_ID=$(grpcurl -plaintext -H "authorization: Bearer ${JWT_TOKEN}" -d '{
    "collection": "posts",
    "limit": "1"
}' "$GRPC_ADDR" crap.ContentAPI/Find 2>/dev/null | jq -r '.documents[0].id // empty')

if [[ -z "$POST_ID" ]]; then
    warn "No posts found in DB — single-item tests will be skipped"
    warn "Run the seed migration first to populate test data"
else
    ok "Using post ID: ${POST_ID}"
fi

# Get the logged-in user's ID for write tests (author field is required on posts)
USER_ID=$(grpcurl -plaintext -d "{\"token\": \"${JWT_TOKEN}\"}" \
    "$GRPC_ADDR" crap.ContentAPI/Me 2>/dev/null | jq -r '.user.id // empty')

if [[ -z "$USER_ID" ]]; then
    warn "Could not get user ID — write tests will be skipped"
else
    ok "Using user ID: ${USER_ID}"
fi

# ── oha test runner ──────────────────────────────────────────

# Run oha and parse JSON output for results
run_oha() {
    local label="$1"
    local concurrency="$2"
    local url="$3"
    shift 3
    local extra_args=("$@")

    info "  ${label} @ c=${concurrency} for ${DURATION}s..."

    local output
    output=$(oha -z "${DURATION}s" -c "$concurrency" \
        --no-tui --output-format json \
        -H "Cookie: ${COOKIE_HEADER}" \
        -H "X-CSRF-Token: ${CSRF_TOKEN}" \
        ${extra_args[@]+"${extra_args[@]}"} \
        "$url" 2>&1) || true

    local rps p50 p95 p99 errors
    rps=$(echo "$output" | jq -r '.summary.requestsPerSec // 0' 2>/dev/null | xargs printf "%.1f" 2>/dev/null || echo "?")
    p50=$(echo "$output" | jq -r '.latencyPercentiles.p50 // "?"' 2>/dev/null || echo "?")
    p95=$(echo "$output" | jq -r '.latencyPercentiles.p95 // "?"' 2>/dev/null || echo "?")
    p99=$(echo "$output" | jq -r '.latencyPercentiles.p99 // "?"' 2>/dev/null || echo "?")

    # Total requests = sum of all status code counts
    # Error requests = status codes 4xx/5xx + connection errors
    local total status_err connect_err
    total=$(echo "$output" | jq -r '
        [.statusCodeDistribution // {} | to_entries[] | .value] | add // 0
    ' 2>/dev/null || echo "0")
    status_err=$(echo "$output" | jq -r '
        [.statusCodeDistribution // {} | to_entries[]
         | select(.key | test("^[45]")) | .value] | add // 0
    ' 2>/dev/null || echo "0")
    connect_err=$(echo "$output" | jq -r '
        [.errorDistribution // {} | to_entries[]
         | select(.key | test("aborted due to deadline") | not)
         | .value] | add // 0
    ' 2>/dev/null || echo "0")
    local all_err=$((status_err + connect_err))

    if [[ "$total" -gt 0 && "$all_err" -gt 0 ]]; then
        local total_with_err=$((total + connect_err))
        errors=$(awk "BEGIN { printf \"%.2f%%\", ($all_err / $total_with_err) * 100 }")
    else
        errors="0"
    fi

    # Format latencies — oha outputs seconds, convert to ms if numeric
    format_latency() {
        local val="$1"
        if [[ "$val" =~ ^[0-9.]+$ ]]; then
            awk "BEGIN { v = $val * 1000; if (v < 1) printf \"%.2fms\", v; else if (v < 100) printf \"%.1fms\", v; else printf \"%.0fms\", v }"
        else
            echo "$val"
        fi
    }

    p50=$(format_latency "$p50")
    p95=$(format_latency "$p95")
    p99=$(format_latency "$p99")

    add_result "$label" "$concurrency" "$rps" "$p50" "$p95" "$p99" "$errors"
}

# ── gRPC load test runner ────────────────────────────────────

# Since oha doesn't speak protobuf, use parallel grpcurl loops.
# Less precise (no percentile latency), but sufficient for finding
# concurrency bugs and measuring throughput.
run_grpc() {
    local label="$1"
    local concurrency="$2"
    local rpc="$3"
    local payload="$4"
    local auth="${5:-}"

    info "  ${label} @ c=${concurrency} for ${DURATION}s..."

    local tmpdir
    tmpdir=$(mktemp -d)
    local end_time=$((SECONDS + DURATION))

    # Launch $concurrency parallel workers
    for i in $(seq 1 "$concurrency"); do
        (
            count=0 errs=0
            while [[ $SECONDS -lt $end_time ]]; do
                if [[ -n "$auth" ]]; then
                    grpcurl -plaintext -H "authorization: Bearer ${auth}" \
                        -d "$payload" "$GRPC_ADDR" "crap.ContentAPI/${rpc}" \
                        >/dev/null 2>&1 && count=$((count + 1)) || errs=$((errs + 1))
                else
                    grpcurl -plaintext \
                        -d "$payload" "$GRPC_ADDR" "crap.ContentAPI/${rpc}" \
                        >/dev/null 2>&1 && count=$((count + 1)) || errs=$((errs + 1))
                fi
            done
            echo "${count} ${errs}" > "${tmpdir}/${i}"
        ) &
    done
    wait

    # Aggregate results
    local total_ok=0 total_err=0
    for f in "${tmpdir}"/*; do
        [[ -e "$f" ]] || continue
        read -r ok_count err_count < "$f"
        total_ok=$((total_ok + ok_count))
        total_err=$((total_err + err_count))
    done
    rm -rf "$tmpdir"

    local total=$((total_ok + total_err))
    local rps="0.0"
    local errors="0"

    if [[ $DURATION -gt 0 ]]; then
        rps=$(awk "BEGIN { printf \"%.1f\", $total / $DURATION }")
    fi
    if [[ $total -gt 0 && $total_err -gt 0 ]]; then
        errors=$(awk "BEGIN { printf \"%.2f%%\", ($total_err / $total) * 100 }")
    fi

    # No per-request latency from grpcurl loops
    add_result "$label" "$concurrency" "$rps" "-" "-" "-" "$errors"
}

# ── gRPC write test (create + delete pairs) ──────────────────

run_grpc_write() {
    local label="$1"
    local concurrency="$2"
    local auth="$3"

    info "  ${label} @ c=${concurrency} for ${DURATION}s..."

    local tmpdir
    tmpdir=$(mktemp -d)
    local end_time=$((SECONDS + DURATION))

    for i in $(seq 1 "$concurrency"); do
        (
            count=0 errs=0
            n=0
            while [[ $SECONDS -lt $end_time ]]; do
                n=$((n + 1))
                slug="loadtest-${i}-${n}-$(date +%s%N)"
                # Create (posts requires: title, slug, post_type, author)
                resp=$(grpcurl -plaintext -H "authorization: Bearer ${auth}" -d "{
                    \"collection\": \"posts\",
                    \"data\": {
                        \"title\": \"Load Test ${slug}\",
                        \"slug\": \"${slug}\",
                        \"post_type\": \"article\",
                        \"author\": \"${USER_ID}\",
                        \"content\": \"Load test document.\"
                    }
                }" "$GRPC_ADDR" crap.ContentAPI/Create 2>&1)

                doc_id=$(echo "$resp" | jq -r '.document.id // empty' 2>/dev/null)

                if [[ -z "$doc_id" ]]; then
                    errs=$((errs + 1))
                    continue
                fi

                # Delete
                grpcurl -plaintext -H "authorization: Bearer ${auth}" -d "{
                    \"collection\": \"posts\",
                    \"id\": \"${doc_id}\"
                }" "$GRPC_ADDR" crap.ContentAPI/Delete >/dev/null 2>&1 \
                    && count=$((count + 1)) || errs=$((errs + 1))
            done
            echo "${count} ${errs}" > "${tmpdir}/${i}"
        ) &
    done
    wait

    local total_ok=0 total_err=0
    for f in "${tmpdir}"/*; do
        [[ -e "$f" ]] || continue
        read -r ok_count err_count < "$f"
        total_ok=$((total_ok + ok_count))
        total_err=$((total_err + err_count))
    done
    rm -rf "$tmpdir"

    local total=$((total_ok + total_err))
    local rps="0.0"
    local errors="0"

    if [[ $DURATION -gt 0 ]]; then
        # Each "op" is a create+delete pair
        rps=$(awk "BEGIN { printf \"%.1f\", $total / $DURATION }")
    fi
    if [[ $total -gt 0 && $total_err -gt 0 ]]; then
        errors=$(awk "BEGIN { printf \"%.2f%%\", ($total_err / $total) * 100 }")
    fi

    add_result "$label" "$concurrency" "${rps}p" "-" "-" "-" "$errors"
}

# ── Scenario runners ─────────────────────────────────────────

scenario_read_list() {
    header "Scenario: Admin list (GET /admin/collections/posts)"
    for c in $CONCURRENCY_LEVELS; do
        run_oha "read_list" "$c" "${ADMIN_URL}/admin/collections/posts"
    done
}

scenario_read_single() {
    if [[ -z "${POST_ID:-}" ]]; then
        warn "Skipping read_single — no posts in DB"
        return
    fi
    header "Scenario: Admin edit form (GET /admin/collections/posts/{id})"
    for c in $CONCURRENCY_LEVELS; do
        run_oha "read_single" "$c" "${ADMIN_URL}/admin/collections/posts/${POST_ID}"
    done
}

scenario_grpc_find() {
    header "Scenario: gRPC Find (depth=0, limit=10)"
    local payload='{"collection":"posts","limit":"10","depth":"0"}'
    for c in $CONCURRENCY_LEVELS; do
        run_grpc "grpc_find" "$c" "Find" "$payload" "$JWT_TOKEN"
    done
}

scenario_grpc_find_deep() {
    header "Scenario: gRPC Find deep (depth=2)"
    local payload='{"collection":"posts","depth":"2"}'
    for c in $CONCURRENCY_LEVELS; do
        run_grpc "grpc_find_deep" "$c" "Find" "$payload" "$JWT_TOKEN"
    done
}

scenario_grpc_write() {
    if [[ -z "${USER_ID:-}" ]]; then
        warn "Skipping grpc_write — no user ID available"
        return
    fi
    header "Scenario: gRPC Create+Delete pairs (write path)"
    for c in $CONCURRENCY_LEVELS; do
        run_grpc_write "grpc_write" "$c" "$JWT_TOKEN"
    done
}

scenario_search() {
    header "Scenario: Admin search API (GET /admin/api/search/posts?q=...)"
    for c in $CONCURRENCY_LEVELS; do
        run_oha "search" "$c" "${ADMIN_URL}/admin/api/search/posts?q=rust"
    done
}

# ── Run scenarios ────────────────────────────────────────────

echo ""
info "Configuration: duration=${DURATION}s, concurrency=[${CONCURRENCY_LEVELS}]"
info "Scenarios: ${SCENARIOS}"

for scenario in $SCENARIOS; do
    case "$scenario" in
        read_list)       scenario_read_list ;;
        read_single)     scenario_read_single ;;
        grpc_find)       scenario_grpc_find ;;
        grpc_find_deep)  scenario_grpc_find_deep ;;
        grpc_write)      scenario_grpc_write ;;
        search)          scenario_search ;;
        *)               warn "Unknown scenario: $scenario" ;;
    esac
done

# ── Summary ──────────────────────────────────────────────────

print_summary
