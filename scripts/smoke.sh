#!/usr/bin/env bash
#
# Smoke test for the container image.
#
# Verifies the things that must hold with no network at all: the binary starts,
# /healthz answers, every protected route rejects an anonymous caller, and a
# CENC-encrypted cache entry is decrypted per Range instead of being served as
# ciphertext.
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
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SEED_DIR=""

cleanup() {
  "$DOCKER" rm -f "$NAME" >/dev/null 2>&1 || true
  "$DOCKER" volume rm "$VOLUME" >/dev/null 2>&1 || true
  if [ -n "${SEED_DIR:-}" ]; then
    rm -rf "$SEED_DIR"
  fi
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

# --- P1-7: encrypted cache entries are decrypted while served ---------------
# Reuse the fixture the Rust tests use: encrypted MP4 bytes, the content key
# and the independently produced plaintext. The image needs no network for
# this, and the fixture keeps the check tied to the same bytes as `cargo test`.
if command -v python3 >/dev/null 2>&1; then
  SEED_DIR="$(mktemp -d)"
  python3 - "$ROOT/crates/fqapi-core/testdata/cenc_sample_fixture.json" "$SEED_DIR" <<'PY'
import base64
import json
import pathlib
import sys

fixture, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
cenc = json.loads(fixture.read_text(encoding="utf-8"))["cenc"]
decode = lambda value: base64.b64decode(value)
encrypted = decode(cenc["full_sample_b64"])
plain = b"".join(decode(sample) for sample in cenc["full_sample_plain_b64"])
(out / "vid.mp4").write_bytes(encrypted)
(out / "vid.mp4.key").write_bytes(bytes.fromhex(cenc["key_hex"]).hex().encode())
(out / "expected.bin").write_bytes(plain)
(out / "meta").write_text(f'{cenc["full_mdat_offset"]} {len(encrypted)}\n', encoding="utf-8")
PY
  # `docker cp` preserves the host mode; a restrictive umask would otherwise
  # hand the container files its unprivileged user cannot read.
  chmod -R a+rX "$SEED_DIR"
  read -r MDAT ENC_LEN < "$SEED_DIR/meta"

  "$DOCKER" exec "$NAME" mkdir -p /data/src
  "$DOCKER" cp "$SEED_DIR/vid.mp4" "$NAME:/data/src/vid.mp4"
  "$DOCKER" cp "$SEED_DIR/vid.mp4.key" "$NAME:/data/src/vid.mp4.key"

  code="$(curl -s -o "$SEED_DIR/actual.bin" -w '%{http_code}' \
    -H "Authorization: Bearer $TOKEN" \
    -H "Range: bytes=$MDAT-$((ENC_LEN - 1))" \
    "$BASE/src/vid.mp4" || true)"
  [ "$code" = "206" ] || fail "encrypted media range returned $code, expected 206"
  cmp -s "$SEED_DIR/expected.bin" "$SEED_DIR/actual.bin" \
    || fail "the served range is not the decrypted plaintext"
  echo "ok   encrypted media is decrypted per Range"

  # Without the sidecar key the server must refuse rather than hand out
  # ciphertext with a success status.
  "$DOCKER" cp "$SEED_DIR/vid.mp4" "$NAME:/data/src/no-key.mp4"
  code="$(status -H "Authorization: Bearer $TOKEN" "$BASE/src/no-key.mp4" || true)"
  [ "$code" = "503" ] || fail "encrypted media without a key returned $code, expected 503"
  echo "ok   encrypted media without a key returns 503"
else
  echo "skip encrypted-media smoke: python3 not found"
fi

ready="$(status "$BASE/readyz" || true)"
echo "info /readyz $ready (200 = the device pool registered upstream)"

echo "==> smoke passed"
