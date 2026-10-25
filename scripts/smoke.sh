#!/usr/bin/env bash
#
# Smoke test for the container image.
#
# Verifies the things that must hold with no network at all: the binary starts,
# /healthz answers, and every protected route rejects an anonymous caller.
#
# It deliberately does NOT fail when /readyz stays 503: readiness waits for the
# device pool to register upstream, which needs network and real device
# identities. The status is reported so a live run can be told apart from an
# offline one.
#
# Usage: scripts/smoke.sh [image]        (default: fqapi-lite-api:local)
#   DOCKER=/path/to/docker   override the CLI
#   PORT=18931               override the published port

set -euo pipefail

# Git Bash rewrites arguments that look like Unix paths; `/data` must reach the
# container unchanged.
export MSYS_NO_PATHCONV=1

IMAGE="${1:-fqapi-lite-api:local}"
DOCKER="${DOCKER:-docker}"
PORT="${PORT:-18931}"
NAME="fqapi-smoke-$$"
VOLUME="fqapi-smoke-$$"
TOKEN="smoke-token"
BASE="http://127.0.0.1:$PORT"

cleanup() {
  "$DOCKER" rm -f "$NAME" >/dev/null 2>&1 || true
  "$DOCKER" volume rm "$VOLUME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  echo "--- container log ---" >&2
  "$DOCKER" logs "$NAME" 2>&1 | tail -20 >&2 || true
  exit 1
}

status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

echo "==> starting $IMAGE on 127.0.0.1:$PORT"
"$DOCKER" volume create "$VOLUME" >/dev/null
# `-e FQ_UPSTREAM_ORIGIN` (no value) forwards the caller's variable, so CI can
# point the container at an unroutable origin and keep the run offline. Unset
# locally means the container talks to the real upstream.
"$DOCKER" run -d --name "$NAME" \
  -p "127.0.0.1:$PORT:18921" \
  -v "$VOLUME:/data" \
  -e "FQ_API_TOKENS=$TOKEN" \
  -e FQ_UPSTREAM_ORIGIN \
  "$IMAGE" --data-dir /data --listen 0.0.0.0:18921 >/dev/null

for _ in $(seq 1 30); do
  [ "$("$DOCKER" inspect -f '{{.State.Running}}' "$NAME")" = "true" ] \
    || fail "the container exited during startup"
  [ "$(status "$BASE/healthz" || true)" = "200" ] && break
  sleep 1
done

code="$(status "$BASE/healthz" || true)"
[ "$code" = "200" ] || fail "/healthz returned $code, expected 200"
echo "ok   /healthz 200 without a token"

code="$(status "$BASE/api/v1/search?query=x" || true)"
[ "$code" = "401" ] || fail "anonymous /api/v1/search returned $code, expected 401"
echo "ok   /api/v1/search 401 without a token"

code="$(status -H "Authorization: Bearer $TOKEN" "$BASE/api/v1/search?query=x" || true)"
[ "$code" != "401" ] || fail "a valid token was rejected"
echo "ok   /api/v1/search $code with a token"

code="$(status "$BASE/src/missing.mp4" || true)"
[ "$code" = "401" ] || fail "anonymous media returned $code, expected 401"
echo "ok   /src/* 401 without a token"

code="$(status "$BASE/api/v1/search?query=x&token=$TOKEN" || true)"
[ "$code" = "401" ] || fail "the query token opened an API route ($code)"
echo "ok   ?token= does not open API routes"

ready="$(status "$BASE/readyz" || true)"
echo "info /readyz $ready (200 = the device pool registered upstream)"

echo "==> smoke passed"
