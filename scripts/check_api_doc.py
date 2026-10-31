#!/usr/bin/env python3
"""Checks `docs/API.md` against the Rust source it claims to describe.

A hand-written API table drifts in two directions, and both are silent:

* fabrication - a path or parameter that does not exist. The server answers 404
  (or ignores the parameter), and the reader concludes the server is broken.
* omission - a real route or a required parameter that never made it into the
  table. Nothing fails; callers just cannot use the feature.

The document is compared with independent sources in the source tree, so a
mistake in any one of them shows up as a mismatch instead of as a
plausible-looking wrong answer:

* path segments   <- the literal arms in `endpoints/router.rs`
* registered apis <- the `add_route("...")` calls in each endpoint module
* query params    <- the `params.get_str("...")` calls in each module
* body params     <- the fields of the module's request struct
* required params <- the "missing parameter" error messages in each module

The optional `--base-url` mode adds the only authoritative existence check there
is: every documented path is requested and must not answer 404.

Usage:
    python scripts/check_api_doc.py
    python scripts/check_api_doc.py --base-url http://127.0.0.1:18921 --token <token>

Exits non-zero on any mismatch.
"""

from __future__ import annotations

import argparse
import re
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Set

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "API.md"
ENDPOINTS = ROOT / "crates" / "fqapi-core" / "src" / "endpoints"

# The router names these but nothing registers them, so they answer
# `404 unsupported api type`. Upstream (`fqapp/rust`) has the same two holes, so
# this is a ported defect, not a porting mistake. Pinned rather than ignored: a
# third dead route fails the check, and implementing one of these also fails it
# until the document is updated.
KNOWN_UNIMPLEMENTED = {"search_predict", "toutiao_article"}

# Params the router injects from the path before the handler runs. They never
# appear in a handler's `get_str` calls.
ROUTER_INJECTED = {
    "item_ids", "item_id", "book_id", "series_id", "video_id", "pseries_id",
    "author_id", "rank_id", "post_id", "comment_id", "vid", "para_index",
    "group_id", "album_id", "api_type", "session_id", "target",
}

# Which endpoint module backs each section of the document. Hand-written on
# purpose: deriving it from the paths would need the router's control flow, and
# a wrong derivation here would silently weaken the parameter checks.
#
# `comments.rs` appears in three sections because the book/chapter review routes
# live there, not in `book.rs` - the path prefix does not decide the module.
SECTION_MODULES = {
    "首页与推荐": ["home.rs", "video.rs"],
    "搜索与分类": ["search.rs"],
    "书籍": ["book.rs", "comments.rs"],
    "正文": ["content.rs", "full.rs", "dhcontent.rs", "toutiao.rs", "audio.rs", "audio_text.rs", "comments.rs"],
    "短剧": ["video.rs", "book.rs", "comments.rs"],
    "评论": ["comments.rs"],
    "漫画": ["manga.rs", "video.rs"],
    "听书": ["audio.rs", "wkcontent.rs"],
    "作者与榜单": ["author.rs"],
    "其它": ["book.rs", "toutiao.rs"],
    "账号采集（默认不对外推荐）": ["account_library.rs", "account_history.rs"],
}

# `/api/v1/account/*` is answered by `dispatch` before the router runs, so its
# segments are not in `router.rs` at all.
PRE_DISPATCH_SECTIONS = {"账号采集（默认不对外推荐）"}

PROBE_ID = "7143038691944959011"


@dataclass
class Route:
    section: str
    method: str
    path: str
    required: Set[str] = field(default_factory=set)
    optional: Set[str] = field(default_factory=set)
    notes: str = ""

    @property
    def all_params(self) -> Set[str]:
        return self.required | self.optional

    @property
    def unimplemented(self) -> bool:
        """The document marks a route the router knows but nothing handles."""
        return "未实现" in self.notes

    @property
    def placeholders(self) -> Set[str]:
        return {
            name
            for name in re.findall(r"\{([a-z_]+)\}", self.path)
            if len(name) >= 3  # `{n}` is a position, not a parameter name
        }


def segments_of(path: str) -> List[str]:
    """Segments after `/api/v1`, with placeholders removed."""
    trimmed = path
    for prefix in ("/api/v1/", "/api/v1"):
        if trimmed.startswith(prefix):
            trimmed = trimmed[len(prefix) :]
            break
    return [part for part in trimmed.strip("/").split("/") if part and not part.startswith("{")]


def parse_doc(text: str) -> List[Route]:
    routes: List[Route] = []
    section = ""
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("## "):
            section = stripped[3:].strip()
            continue
        if not stripped.startswith("|"):
            continue
        cells = [cell.strip() for cell in stripped.strip("|").split("|")]
        if len(cells) < 3 or cells[0] not in ("GET", "POST"):
            continue
        path = re.search(r"`(/api/v1[^`]*)`", cells[1])
        if not path:
            continue
        required = set(re.findall(r"`([a-z_][a-z0-9_]*)`", cells[2]))
        optional = set()
        if len(cells) >= 5:
            optional = set(re.findall(r"`([a-z_][a-z0-9_]*)`", cells[3]))
        routes.append(Route(section, cells[0], path.group(1), required, optional, cells[-1]))
    return routes


def router_segments(text: str) -> Set[str]:
    """Literal path segments in `router.rs`.

    They appear as `"detail" => ...` arms (including `"a" | "b" =>`) and as
    `parts[n] == "add"` guards. Endpoint *names* (`api: "book_share"`) are not
    segments and are not collected.
    """
    segments: Set[str] = set()
    for match in re.finditer(r'((?:"[a-z0-9-]+"\s*(?:\|\s*)?)+)=>\s*', text):
        segments.update(re.findall(r'"([a-z0-9-]+)"', match.group(1)))
    segments.update(re.findall(r'==\s*"([a-z0-9-]+)"', text))
    return segments


def routed_apis(text: str) -> Set[str]:
    return set(re.findall(r'api:\s*"([a-z_0-9]+)"', text))


def registered_apis(directory: Path) -> Dict[str, str]:
    """api name -> module file name."""
    found: Dict[str, str] = {}
    for path in directory.glob("*.rs"):
        for api in re.findall(r'add_route\(\s*"([a-z_0-9]+)"', path.read_text(encoding="utf-8")):
            found[api] = path.name
    return found


def module_params(path: Path) -> Set[str]:
    """Query params plus request-body fields a module reads."""
    text = path.read_text(encoding="utf-8")
    params = set(re.findall(r'get_str\("([a-z_0-9]+)"\)', text))
    # Account capture takes its input as a JSON body deserialised into a struct.
    body = re.search(r"struct CaptureRequest\s*\{(.*?)\n\}", text, re.S)
    if body:
        params |= set(re.findall(r"^\s*([a-z_][a-z0-9_]*)\s*:", body.group(1), re.M))
    return params


def module_required(path: Path, params: Set[str]) -> Set[str]:
    """Parameters named in this module's "missing parameter" errors.

    The messages are prose ("缺少参数: book_id 或 group_id 至少需要一个"), so the
    identifiers are filtered against the params the module actually reads -
    otherwise words like `parameter` would be picked up.
    """
    text = path.read_text(encoding="utf-8")
    messages = re.findall(r'"([^"]*(?:缺少|missing|请提供|不能为空)[^"]*)"', text)
    # Only a message that talks about a *parameter* declares one as required.
    # Data-level prose ("cached video key missing", "book detail content
    # missing") can name a query parameter by coincidence and must not turn it
    # into a documented requirement.
    messages = [message for message in messages if "参数" in message or "parameter" in message]
    required: Set[str] = set()
    for message in messages:
        required.update(
            word for word in re.findall(r"[a-z_][a-z0-9_]{3,}", message) if word in params
        )
    return required


def probe(base_url: str, token: str, route: Route) -> int:
    path = re.sub(r"\{[a-z_]+\}", PROBE_ID, route.path)
    request = urllib.request.Request(f"{base_url.rstrip('/')}{path}", method=route.method)
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status
    except urllib.error.HTTPError as exc:
        return exc.code
    except urllib.error.URLError as exc:
        raise SystemExit(f"cannot reach {base_url}: {exc.reason}") from None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", help="also probe every documented path")
    parser.add_argument("--token", default="", help="token for --base-url")
    args = parser.parse_args()

    routes = parse_doc(DOC.read_text(encoding="utf-8"))
    if not routes:
        print("FAIL: no routes parsed from docs/API.md", file=sys.stderr)
        return 1

    router_text = (ENDPOINTS / "router.rs").read_text(encoding="utf-8")
    known_segments = router_segments(router_text)
    documented_segments = {
        segment
        for route in routes
        if route.section not in PRE_DISPATCH_SECTIONS
        for segment in segments_of(route.path)
    }

    registered = registered_apis(ENDPOINTS)
    routed = routed_apis(router_text)

    modules = {module for mods in SECTION_MODULES.values() for module in mods}
    params_by_module = {module: module_params(ENDPOINTS / module) for module in modules}
    required_by_module = {
        module: module_required(ENDPOINTS / module, params_by_module[module])
        for module in modules
    }

    problems: List[str] = []

    # 1. Every segment the document uses must exist in the router.
    for segment in sorted(documented_segments - known_segments):
        users = [r.path for r in routes if segment in segments_of(r.path)]
        problems.append(f"documented segment `{segment}` is not in router.rs (used by {users})")

    # 2. Every segment the router knows must appear in the document.
    for segment in sorted(known_segments - documented_segments):
        problems.append(f"router segment `{segment}` is not documented")

    # 3. Every routed endpoint must be registered, except the known holes.
    unregistered = routed - set(registered)
    for api in sorted(unregistered - KNOWN_UNIMPLEMENTED):
        problems.append(f"router references `{api}` but no module registers it (answers 404)")
    for api in sorted(KNOWN_UNIMPLEMENTED - unregistered):
        problems.append(
            f"`{api}` is in KNOWN_UNIMPLEMENTED but is now registered - "
            f"update the list and the document"
        )

    # 3b. A route the document marks unimplemented must be one of those holes,
    #     and must not advertise parameters it cannot honour.
    documented_holes = [route for route in routes if route.unimplemented]
    for route in documented_holes:
        if route.all_params:
            problems.append(
                f"{route.method} {route.path}: marked unimplemented but documents parameters"
            )
    if len(documented_holes) != len(KNOWN_UNIMPLEMENTED):
        problems.append(
            f"{len(documented_holes)} route(s) marked unimplemented in the document, "
            f"but KNOWN_UNIMPLEMENTED lists {len(KNOWN_UNIMPLEMENTED)} - keep them in sync"
        )

    # 4. Documented parameters must be ones some module actually reads, or ones
    #    the router injects, or the route's own placeholders.
    all_module_params = set().union(*params_by_module.values()) | ROUTER_INJECTED
    for route in routes:
        for param in sorted(route.all_params - all_module_params):
            problems.append(f"{route.method} {route.path}: parameter `{param}` is read by no module")

    # 5. Within a section, parameters must belong to that section's modules.
    for route in routes:
        modules_for_section = SECTION_MODULES.get(route.section)
        if modules_for_section is None:
            problems.append(f"section `{route.section}` has no module mapping")
            continue
        allowed = (
            set().union(*(params_by_module[m] for m in modules_for_section))
            | ROUTER_INJECTED
            | route.placeholders
        )
        for param in sorted(route.all_params - allowed):
            problems.append(
                f"{route.method} {route.path}: `{param}` is not read by "
                f"{'/'.join(modules_for_section)} (section `{route.section}`)"
            )

    # 6. A parameter a module declares mandatory must be documented somewhere -
    #    as a query parameter or as a path placeholder.
    documented_params = set().union(
        *(route.all_params | route.placeholders for route in routes)
    )
    for module, required in required_by_module.items():
        for param in sorted(required - documented_params):
            problems.append(f"{module} requires `{param}` but no documented route lists it")

    # 7. Optional existence check against a live server.
    probed = 0
    if args.base_url:
        for route in routes:
            if route.unimplemented:
                continue  # documented as 404 on purpose
            status = probe(args.base_url, args.token, route)
            probed += 1
            if status == 404:
                problems.append(f"{route.method} {route.path}: documented but the server answers 404")

    # 8. The same route documented twice is almost always a copy-paste slip.
    seen: Dict[str, int] = {}
    for route in routes:
        seen[route.path] = seen.get(route.path, 0) + 1
    for path, count in sorted(seen.items()):
        if count > 1:
            problems.append(f"{path} is documented {count} times")

    print(f"documented routes : {len(routes)}")
    print(f"router segments   : {len(known_segments)}")
    print(f"registered apis   : {len(registered)} (routed {len(routed)})")
    print(f"modules           : {len(params_by_module)}")
    if args.base_url:
        print(f"probed            : {probed}")

    if problems:
        print(f"\n{len(problems)} problem(s):", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1

    print("\nOK: docs/API.md matches the source")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
