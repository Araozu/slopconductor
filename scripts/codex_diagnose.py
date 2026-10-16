#!/usr/bin/env python3
"""Probe the documented ChatGPT plan route without printing credentials.

Makes one small text request. It never refreshes or changes saved credentials.
Run on the daemon host when its provider error does not establish the cause.
"""

import argparse
import json
import os
from pathlib import Path
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import zlib


MAX_BODY_BYTES = 64 * 1024


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        # Match Hive's transport and never forward a bearer token to a redirect.
        return None


def endpoint_without_query(url):
    parsed = urllib.parse.urlsplit(url)
    return f"{parsed.scheme}://{parsed.hostname or ''}{parsed.path}"


def response_events(lines):
    """Handle standard SSE frames, including multi-line data fields."""
    data = []
    for line in lines:
        if len(line) > MAX_BODY_BYTES:
            raise ValueError("event line exceeds diagnostic bound")
        line = line.strip(b"\r\n")
        if line.startswith(b"data:"):
            data.append(line[5:].lstrip(b" "))
            if sum(map(len, data)) > MAX_BODY_BYTES:
                raise ValueError("event exceeds diagnostic bound")
        elif not line and data:
            try:
                yield json.loads(b"\n".join(data))
            except ValueError:
                pass
            data = []


def stream_shape(event):
    """Schema identifiers only; exclude text, arguments, tokens and item IDs."""
    def kind(value):
        return value if isinstance(value, str) and re.fullmatch(r"[a-z][a-z0-9_.-]{0,95}", value) else "<unrecognized>"

    result = {"event": kind(event.get("type"))}
    if isinstance(event.get("item"), dict):
        result["item_type"] = kind(event["item"].get("type"))
    if isinstance(event.get("part"), dict):
        result["part_type"] = kind(event["part"].get("type"))
    response = event.get("response")
    if isinstance(response, dict) and isinstance(response.get("output"), list):
        result["output_types"] = [kind(item.get("type")) for item in response["output"] if isinstance(item, dict)]
        result["content_types"] = [kind(part.get("type")) for item in response["output"] if isinstance(item, dict) and isinstance(item.get("content"), list) for part in item["content"] if isinstance(part, dict)]
    return result


def nonstream_summary(response, diagnostic):
    """Inspect a bounded response without dumping HTML or generated content."""
    raw = response.read(MAX_BODY_BYTES + 1)
    truncated = len(raw) > MAX_BODY_BYTES
    raw = raw[:MAX_BODY_BYTES]
    encoding = response.headers.get("Content-Encoding", "").strip().lower()
    if encoding in ("gzip", "deflate"):
        decoder = zlib.decompressobj(31 if encoding == "gzip" else zlib.MAX_WBITS)
        raw = decoder.decompress(raw, MAX_BODY_BYTES + 1)
        truncated = truncated or len(raw) > MAX_BODY_BYTES
        raw = raw[:MAX_BODY_BYTES]
    elif encoding and encoding != "identity":
        print(f"Unsupported content encoding: {encoding!r}; response body omitted.")
        return None
    print(f"Body bytes inspected: {len(raw)}; truncated: {truncated}", flush=True)
    if re.match(rb"(?:event|data):", raw.lstrip()):
        print("Body has SSE framing despite its Content-Type.", flush=True)
        return response_events(raw.splitlines(keepends=True))
    try:
        body = json.loads(raw)
    except ValueError:
        if raw.lstrip().lower().startswith((b"<!doctype html", b"<html")):
            print("Body is an HTML page; HTML and cookies omitted.", flush=True)
            text = raw.decode("utf-8", errors="replace").lower()
            for marker in ("cloudflare", "just a moment", "access denied", "sign in", "log in"):
                if marker in text:
                    print(f"HTML marker: {marker}", flush=True)
        else:
            print("Body is neither JSON nor SSE; raw content omitted.", flush=True)
        return None
    if isinstance(body, dict):
        known_keys = ("error", "detail", "status", "object", "output", "type", "response", "message", "code")
        print("JSON fields present: " + ", ".join(key for key in known_keys if key in body), flush=True)
        if body.get("status") in ("completed", "failed", "incomplete", "queued", "in_progress"):
            print(f"JSON response status: {body['status']}", flush=True)
        diagnostic(body)
    else:
        print("Body is JSON but is not a Responses object.", flush=True)
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--account")
    parser.add_argument("--model", default="gpt-6.1-sol")
    parser.add_argument("--event-types", action="store_true", help="Report stream event and output schema types without generated text.")
    args = parser.parse_args()
    if args.data_dir:
        data = args.data_dir
    elif os.environ.get("SLOP_DATA_DIR"):
        data = Path(os.environ["SLOP_DATA_DIR"])
    elif os.name == "nt":
        data = Path(os.environ["LOCALAPPDATA"]) / "slopconductor"
    else:
        xdg = os.environ.get("XDG_DATA_HOME", "")
        data = (Path(xdg) if Path(xdg).is_absolute() else Path.home() / ".local/share") / "slopconductor"
    credentials = data / "credentials"
    account = args.account
    selection = credentials / "codex-auth-mode"
    if account is None and selection.exists():
        try:
            account = json.loads(selection.read_text()).get("account_id")
        except (ValueError, AttributeError):
            pass
    account = (account or "default").lower()
    if not re.fullmatch(r"[a-z0-9_-]{1,64}", account):
        raise ValueError("invalid account label")
    path = credentials / ("codex-chatgpt.json" if account == "default" else f"codex-chatgpt-{account}.json")
    saved = json.loads(path.read_text())
    token = saved.get("access_token")
    if not token:
        raise ValueError("selected registration is signed out")
    secrets = [saved.get(key, "") for key in ("id_token", "access_token", "refresh_token")]

    def diagnostic(body):
        nested = body.get("response")
        error = body.get("error", nested.get("error", {}) if isinstance(nested, dict) else {})
        if not isinstance(error, dict):
            error = {}
        result = {key: error.get(key) for key in ("code", "type", "param") if error.get(key) is not None}
        message = error.get("message", body.get("detail", body.get("message")))
        if isinstance(message, str):
            for secret in secrets:
                if secret:
                    message = message.replace(secret, "[redacted]")
            result["message"] = message[:2000]
        rendered = json.dumps(result, ensure_ascii=True)
        for secret in secrets:
            if secret:
                rendered = rendered.replace(secret, "[redacted]")
        print(rendered, flush=True)

    payload = {"model": args.model, "input": [{"role": "user", "content": "Reply with exactly: ok"}], "store": False, "stream": True}
    request = urllib.request.Request("https://api.openai.com/v1/responses", data=json.dumps(payload).encode(),
                                     headers={"Authorization": "Bearer " + token, "Content-Type": "application/json", "Accept": "text/event-stream"})
    print(f"Account: {account}; model: {args.model}; minimal Responses request (no workspace tools)", flush=True)
    try:
        response = urllib.request.build_opener(NoRedirect()).open(request, timeout=120)
    except urllib.error.HTTPError as error:
        print(f"HTTP {error.code}; request ID: {error.headers.get('x-request-id', 'unavailable')}", flush=True)
        print(f"Content-Type: {error.headers.get('Content-Type', '')!r}", flush=True)
        if error.headers.get("Location"):
            print("Redirect target (query omitted): " + endpoint_without_query(error.headers["Location"]), flush=True)
        try:
            diagnostic(json.loads(error.read(16384)))
        except (ValueError, AttributeError):
            print("Provider returned a non-JSON error body (omitted).")
        return 1
    with response:
        print(f"HTTP {response.status}; request ID: {response.headers.get('x-request-id', 'unavailable')}", flush=True)
        content_type = response.headers.get("Content-Type", "")
        print(f"Content-Type: {content_type!r}; Content-Encoding: {response.headers.get('Content-Encoding', 'identity')!r}", flush=True)
        print(f"Final endpoint: {endpoint_without_query(response.geturl())}", flush=True)
        if content_type.split(";", 1)[0].strip().lower() == "text/event-stream":
            def lines():
                for _ in range(10000):
                    line = response.readline(MAX_BODY_BYTES + 1)
                    if not line:
                        return
                    yield line
            events = response_events(lines())
        else:
            print("Inspecting non-SSE response instead of discarding it.", flush=True)
            events = nonstream_summary(response, diagnostic)
            if events is None:
                return 1
        seen_shapes = set()
        for event in events:
            if not isinstance(event, dict):
                continue
            if args.event_types:
                shape = json.dumps(stream_shape(event), ensure_ascii=True, sort_keys=True)
                for secret in secrets:
                    if secret:
                        shape = shape.replace(secret, "[redacted]")
                if shape not in seen_shapes and len(seen_shapes) < 128:
                    print(shape, flush=True)
                    seen_shapes.add(shape)
            kind = event.get("type")
            if kind == "response.completed":
                print("Minimal generation completed successfully.")
                return 0
            if kind in ("response.failed", "response.error", "error", "response.incomplete"):
                print(f"Stream ended with {kind}.")
                diagnostic(event)
                return 1
    print("Stream ended without response.completed.")
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, zlib.error):
        print("Diagnostic could not read the selected credential or reach OpenAI; no credentials were printed.", file=sys.stderr)
        sys.exit(1)
