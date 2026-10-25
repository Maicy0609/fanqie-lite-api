"""Minimal stdlib-only client for fanqie-lite-api.

Two jobs: it is the reference for "how a client talks to this server", and it is
what the end-to-end test drives. Standard library only - a client that needs
`pip install` before it can be read is not a reference.

    from fqapi_client import FqapiClient
    api = FqapiClient("http://127.0.0.1:18921", "your-token")
    api.search("剑来")

The server passes upstream JSON through unchanged, so responses here are plain
dicts with upstream field names. Only `data.content` (decrypted text) and media
paths (`/src/...`) are rewritten by the server.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Dict, Mapping, Optional

DEFAULT_TIMEOUT = 30.0


class FqapiError(RuntimeError):
    """A non-2xx response, or a body that was not JSON."""

    def __init__(self, status: int, message: str) -> None:
        super().__init__(f"HTTP {status}: {message}")
        self.status = status
        self.message = message


class FqapiClient:
    def __init__(
        self,
        base_url: str,
        token: str,
        timeout: float = DEFAULT_TIMEOUT,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout = timeout

    # -- transport ---------------------------------------------------------

    def request(self, path: str, **params: Any) -> Dict[str, Any]:
        """GET `path` and decode the JSON body.

        Empty and `None` parameters are dropped rather than sent as blanks:
        upstream distinguishes "absent" from "empty" on several routes.
        """
        url = self.url(path, params)
        request = urllib.request.Request(
            url, headers={"Authorization": f"Bearer {self.token}"}
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                payload = response.read()
        except urllib.error.HTTPError as exc:
            raise FqapiError(exc.code, _error_message(exc)) from None
        except urllib.error.URLError as exc:
            raise FqapiError(0, f"connection failed: {exc.reason}") from None
        try:
            return json.loads(payload or b"{}")
        except json.JSONDecodeError as exc:
            raise FqapiError(200, f"response was not JSON: {exc}") from None

    def post(self, path: str, body: Mapping[str, Any], **params: Any) -> Dict[str, Any]:
        url = self.url(path, params)
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        request = urllib.request.Request(
            url,
            data=data,
            method="POST",
            headers={
                "Authorization": f"Bearer {self.token}",
                "Content-Type": "application/json; charset=utf-8",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                payload = response.read()
        except urllib.error.HTTPError as exc:
            raise FqapiError(exc.code, _error_message(exc)) from None
        return json.loads(payload or b"{}")

    def url(self, path: str, params: Optional[Mapping[str, Any]] = None) -> str:
        url = f"{self.base_url}/{path.lstrip('/')}"
        pairs = [
            (key, value)
            for key, value in (params or {}).items()
            if value is not None and value != ""
        ]
        if pairs:
            url = f"{url}?{urllib.parse.urlencode(pairs)}"
        return url

    def media_url(self, path: str) -> str:
        """Absolute URL for a `/src/...` path, carrying the token.

        An `<img>` or `<video>` cannot send an `Authorization` header, so the
        server accepts the token in the query for media paths only. A value that
        already has a scheme (a CDN URL) is returned untouched - re-encoding it
        would invalidate the upstream signature.
        """
        if "://" in path:
            return path
        base = self.url(path)
        joiner = "&" if "?" in base else "?"
        return f"{base}{joiner}{urllib.parse.urlencode({'token': self.token})}"

    # -- probes ------------------------------------------------------------

    def healthz(self) -> Dict[str, Any]:
        return self.request("/healthz")

    def readyz(self) -> Dict[str, Any]:
        return self.request("/readyz")

    # -- content -----------------------------------------------------------

    def search(self, query: str, **params: Any) -> Dict[str, Any]:
        return self.request("/api/v1/search", query=query, **params)

    def book(self, book_id: str) -> Dict[str, Any]:
        return self.request(f"/api/v1/books/{book_id}")

    def directory(self, book_id: str) -> Dict[str, Any]:
        return self.request(f"/api/v1/books/{book_id}/directory")

    def chapter(self, item_id: str) -> Dict[str, Any]:
        return self.request(f"/api/v1/chapters/{item_id}")

    def video(self, video_id: str, stream: bool = True) -> Dict[str, Any]:
        """`stream=True` asks for the CDN URL plus `key_hex` and keeps nothing
        on the server; `stream=False` makes the server cache the decrypted
        episode under `/src/`."""
        params = {"mode": "stream"} if stream else {}
        return self.request(f"/api/v1/videos/{video_id}", **params)

    def homepage(self, **params: Any) -> Dict[str, Any]:
        return self.request("/api/v1/recommend/homepage", **params)


def _error_message(exc: urllib.error.HTTPError) -> str:
    """The server's error envelope is `{"success":false,"error":"..."}`."""
    try:
        payload = json.loads(exc.read() or b"{}")
    except (json.JSONDecodeError, OSError):
        return exc.reason or "request failed"
    if isinstance(payload, dict) and payload.get("error"):
        return str(payload["error"])
    return exc.reason or "request failed"


def _main(argv: list[str]) -> int:
    base = os.environ.get("FQ_API_BASE", "http://127.0.0.1:18921")
    token = os.environ.get("FQ_API_TOKEN", "")
    if not token:
        print("set FQ_API_TOKEN (or pass it in code)", file=sys.stderr)
        return 2
    if not argv or argv[0] in ("-h", "--help"):
        print(
            "usage: FQ_API_TOKEN=<token> python fqapi_client.py <command> [arg]\n"
            "commands: healthz readyz search <query> book <id> directory <id>\n"
            "          chapter <id> video <id>",
            file=sys.stderr,
        )
        return 0

    api = FqapiClient(base, token)
    command, args = argv[0], argv[1:]
    try:
        if command == "healthz":
            result = api.healthz()
        elif command == "readyz":
            result = api.readyz()
        elif command == "search":
            result = api.search(args[0])
        elif command == "book":
            result = api.book(args[0])
        elif command == "directory":
            result = api.directory(args[0])
        elif command == "chapter":
            result = api.chapter(args[0])
        elif command == "video":
            result = api.video(args[0])
        else:
            print(f"unknown command: {command}", file=sys.stderr)
            return 2
    except FqapiError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    except IndexError:
        print(f"{command} needs an argument", file=sys.stderr)
        return 2

    # Directory payloads run to hundreds of kilobytes; a terminal dump is not
    # a useful default.
    text = json.dumps(result, ensure_ascii=False, indent=2)
    print(text if len(text) <= 4000 else text[:4000] + "\n... (truncated)")
    return 0


if __name__ == "__main__":
    raise SystemExit(_main(sys.argv[1:]))
