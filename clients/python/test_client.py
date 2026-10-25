"""Tests for the reference client. No network, no server binary.

Run from this directory:

    python -m unittest -v
"""

import json
import threading
import unittest
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from fqapi_client import FqapiClient, FqapiError


class _Recorder(BaseHTTPRequestHandler):
    """Records what the client sent and answers with canned payloads."""

    def log_message(self, *args):  # keep the test output readable
        pass

    def do_GET(self):
        self.server.seen.append(("GET", self.path, dict(self.headers)))
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path == "/unauthorized":
            self._json(401, {"success": False, "error": "missing or invalid API token"})
        elif parsed.path == "/not-json":
            self._raw(200, b"<html>not json</html>")
        else:
            self._json(200, {"code": 0, "path": parsed.path})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        self.server.seen.append(("POST", self.path, dict(self.headers), body))
        self._json(200, {"code": 0, "echo": json.loads(body or b"{}")})

    def _json(self, status, payload):
        self._raw(status, json.dumps(payload).encode("utf-8"))

    def _raw(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class ClientTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), _Recorder)
        cls.server.seen = []
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.server.seen.clear()
        self.api = FqapiClient(self.base, "tok")

    def test_sends_the_bearer_token(self):
        self.api.request("/api/v1/search", query="x")
        method, _, headers = self.server.seen[-1][:3]
        self.assertEqual(method, "GET")
        self.assertEqual(headers.get("Authorization"), "Bearer tok")

    def test_encodes_query_parameters(self):
        self.api.search("剑来", offset=20)
        path = self.server.seen[-1][1]
        query = urllib.parse.parse_qs(urllib.parse.urlparse(path).query)
        self.assertEqual(query["query"], ["剑来"])
        self.assertEqual(query["offset"], ["20"])

    def test_drops_empty_parameters(self):
        self.api.request("/api/v1/search", query="x", offset=None, count="")
        path = self.server.seen[-1][1]
        self.assertNotIn("offset", path)
        self.assertNotIn("count", path)

    def test_maps_the_error_envelope(self):
        with self.assertRaises(FqapiError) as caught:
            self.api.request("/unauthorized")
        self.assertEqual(caught.exception.status, 401)
        self.assertEqual(caught.exception.message, "missing or invalid API token")

    def test_a_non_json_body_is_reported(self):
        with self.assertRaises(FqapiError):
            self.api.request("/not-json")

    def test_media_url_carries_the_token(self):
        url = self.api.media_url("/src/img_abc.jpg")
        self.assertTrue(url.startswith(self.base))
        self.assertIn("token=tok", url)

    def test_media_url_leaves_absolute_urls_alone(self):
        cdn = "https://cdn.example/x.mp4?sign=abc"
        self.assertEqual(self.api.media_url(cdn), cdn)

    def test_post_sends_a_json_body(self):
        self.api.post(
            "/api/v1/account/fanqie/library",
            {"cookie": "c", "resource": "history"},
        )
        method, _, headers, body = self.server.seen[-1]
        self.assertEqual(method, "POST")
        self.assertEqual(json.loads(body), {"cookie": "c", "resource": "history"})
        self.assertIn("application/json", headers.get("Content-Type", ""))


if __name__ == "__main__":
    unittest.main()
