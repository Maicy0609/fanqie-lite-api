"""End-to-end check: real server binary, mock upstream, reference client.

This is the chain the unit tests cannot cover: the client speaks HTTP to the
actual binary, the binary signs a request and sends it to an upstream, and the
response comes back through the same path a real deployment uses.

The upstream is local, so the run is offline and deterministic. That also means
it never registers real device identities - the pool is seeded instead.

    python clients/python/e2e.py                 # auto-detects the binary
    python clients/python/e2e.py --binary path/to/fqapi-server
    python clients/python/e2e.py --skip-doc-check

Exits non-zero on the first failed expectation.
"""

from __future__ import annotations

import argparse
import json
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from fqapi_client import FqapiClient, FqapiError

ROOT = Path(__file__).resolve().parents[2]
TOKEN = "e2e-token"
DEVICE_COUNT = 5


class _MockUpstream(BaseHTTPRequestHandler):
    """Answers every upstream call with one recognisable payload."""

    def log_message(self, *args):
        pass

    def do_GET(self):
        self._answer()

    def do_POST(self):
        self._answer()

    def _answer(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        body = json.dumps(
            {
                "code": 0,
                "data": {"books": [{"book_id": "1", "title": "mock-upstream"}]},
            }
        ).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def find_binary(explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit)
        if not path.exists():
            raise SystemExit(f"binary not found: {path}")
        return path
    suffix = ".exe" if sys.platform == "win32" else ""
    for profile in ("release", "debug"):
        candidate = ROOT / "target" / profile / f"fqapi-server{suffix}"
        if candidate.exists():
            return candidate
    raise SystemExit(
        "no server binary found; run `cargo build -p fqapi-server` or pass --binary"
    )


def seed_data_dir(directory: Path) -> None:
    """A pool of devices means the run never calls device_register."""
    config = directory / "config"
    config.mkdir(parents=True)
    (config / "config.json").write_text('{"algorithm_type":"8404","port":0}\n', encoding="utf-8")
    (config / "filter.json").write_text('{"routes":{}}\n', encoding="utf-8")
    devices = [
        {
            "device_id": f"{i:016}",
            "install_id": str(9_000_000_000_000_000_000 + i),
            "secret_key": f"{i:032x}",
            "platform": "android",
            "status": "active",
            "created_time": "2026-01-01 00:00:00",
            "last_used": f"2026-01-01 00:00:0{i}",
            "use_count": i,
            "cdid": f"cdid-{i}",
            "openudid": f"{i:016}",
        }
        for i in range(DEVICE_COUNT)
    ]
    (config / "device_pool.json").write_text(
        json.dumps({"android": devices, "last_update": "2026-01-01 00:00:00"}),
        encoding="utf-8",
    )
    (directory / "src").mkdir()


def wait_for(url: str, seconds: float) -> bool:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if response.status == 200:
                    return True
        except Exception:  # noqa: BLE001 - any failure just means "not yet"
            time.sleep(0.2)
    return False


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", help="path to fqapi-server")
    parser.add_argument("--skip-doc-check", action="store_true")
    args = parser.parse_args()

    binary = find_binary(args.binary)
    workdir = Path(tempfile.mkdtemp(prefix="fqapi-e2e-"))
    upstream_port = free_port()
    server_port = free_port()
    upstream = ThreadingHTTPServer(("127.0.0.1", upstream_port), _MockUpstream)
    threading.Thread(target=upstream.serve_forever, daemon=True).start()

    server: subprocess.Popen | None = None
    failures: list[str] = []

    def expect(condition: bool, message: str) -> None:
        if condition:
            print(f"ok   {message}")
        else:
            print(f"FAIL {message}", file=sys.stderr)
            failures.append(message)

    try:
        seed_data_dir(workdir)
        server = subprocess.Popen(
            [
                str(binary),
                "--data-dir", str(workdir),
                "--listen", f"127.0.0.1:{server_port}",
                "--token", TOKEN,
                "--upstream-origin", f"http://127.0.0.1:{upstream_port}",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        base = f"http://127.0.0.1:{server_port}"
        if not wait_for(f"{base}/healthz", 30):
            details = ""
            if server.poll() is not None:
                details = server.stderr.read().decode("utf-8", "replace")
            raise SystemExit(f"server did not become healthy\n{details}")

        api = FqapiClient(base, TOKEN)

        expect(api.healthz().get("devices") == DEVICE_COUNT, "healthz reports the seeded pool")
        expect(api.readyz().get("status") == "ok", "readyz is ok with a seeded pool")

        payload = api.search("剑来")
        books = payload.get("data", {}).get("books") or []
        expect(
            books and books[0].get("title") == "mock-upstream",
            "search returns the upstream payload unchanged",
        )

        try:
            FqapiClient(base, "wrong-token").search("x")
            expect(False, "a wrong token is rejected")
        except FqapiError as exc:
            expect(exc.status == 401, "a wrong token is rejected with 401")

        media = workdir / "src" / "clip.mp4"
        media.write_bytes(b"0123456789ABCDEF")
        with urllib.request.urlopen(api.media_url("/src/clip.mp4"), timeout=10) as response:
            expect(response.read() == b"0123456789ABCDEF", "media is served via ?token=")

        request = urllib.request.Request(
            f"{base}/src/clip.mp4", headers={"Range": "bytes=0-3"}
        )
        request.add_header("Authorization", f"Bearer {TOKEN}")
        with urllib.request.urlopen(request, timeout=10) as response:
            expect(
                response.status == 206 and response.read() == b"0123",
                "media honours Range",
            )

        if not args.skip_doc_check:
            result = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "scripts" / "check_api_doc.py"),
                    "--base-url", base,
                    "--token", TOKEN,
                ],
                capture_output=True,
                text=True,
            )
            expect(
                result.returncode == 0,
                "every documented route answers on the live server",
            )
            if result.returncode != 0:
                print(result.stderr, file=sys.stderr)
    finally:
        if server is not None:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
            server.stderr.close()
        upstream.shutdown()
        upstream.server_close()
        shutil.rmtree(workdir, ignore_errors=True)

    if failures:
        print(f"\n{len(failures)} check(s) failed", file=sys.stderr)
        return 1
    print("\n==> e2e passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
