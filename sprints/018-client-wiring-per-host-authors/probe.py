#!/usr/bin/env python3
"""Raw JSON-RPC probe of a kaed endpoint at a chosen protocol revision.

Deliberately NOT an rmcp client. Sprint 016's standing rule is that a green
rmcp-client test says nothing about 2026-07-28 behaviour, and rmcp 3.1.0's
client cannot drive that revision at all (016 D-2) — so anything about
2026-07-28, or about what the gateway does to a response, has to be driven
by hand. This is that hand.

Driving 2026-07-28 needs four things beyond a 2025-11-25 handshake, each of
which is a separate 400 that names only itself:

  1. `Mcp-Method` header on every request after `initialize`
  2. `Mcp-Name` header additionally on `tools/call`, naming the tool
  3. `_meta["io.modelcontextprotocol/protocolVersion"]` in params
  4. `_meta["io.modelcontextprotocol/clientCapabilities"]` in params

Diagnostics go to stderr and the tool's own JSON to stdout, so the result
can be piped straight into jq or python.

usage:  probe.py <url> <token-file> [protocol] [tool_name] [json_args]

  probe.py http://127.0.0.1:4870/mcp ~/.config/kaed/token 2026-07-28
  probe.py http://127.0.0.1:4870/mcp ~/.config/kaed/token 2026-07-28 roots '{}'
"""
import json
import ssl
import sys
import urllib.request

url, token_file = sys.argv[1], sys.argv[2]
proto = sys.argv[3] if len(sys.argv) > 3 else "2026-07-28"
call = sys.argv[4:] if len(sys.argv) > 4 else []

token = open(token_file).read().strip()
# Verification off: this drives tailnet-served endpoints from inside the
# tailnet, where the transport is already authenticated and the cert chain
# available to a bare python process varies by host. Fine for a diagnostic;
# do not copy this into anything that talks to the open internet.
ctx = ssl.create_default_context()
ctx.check_hostname = False
ctx.verify_mode = ssl.CERT_NONE

session = {}


META = {
    "io.modelcontextprotocol/protocolVersion": None,  # filled in below
    "io.modelcontextprotocol/clientCapabilities": {},
    "io.modelcontextprotocol/clientInfo": {"name": "kaed-018-probe", "version": "0"},
}


def rpc(payload, notify=False):
    # 2026-07-28 requires every post-initialize request to restate the
    # protocol version and client capabilities in `_meta`.
    if proto >= "2026-07-28" and payload["method"] != "initialize":
        meta = dict(META, **{"io.modelcontextprotocol/protocolVersion": proto})
        payload.setdefault("params", {})
        if payload["params"] is not None:
            payload["params"]["_meta"] = meta
    body = json.dumps(payload).encode()
    headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
        "Authorization": f"Bearer {token}",
        "MCP-Protocol-Version": proto,
        # 2026-07-28 requires the method be echoed in a header so a proxy can
        # route without parsing the body.
        "Mcp-Method": payload["method"],
    }
    # …and `tools/call` must additionally name the tool in a header.
    if payload["method"] == "tools/call":
        headers["Mcp-Name"] = payload["params"]["name"]
    headers.update(session)
    req = urllib.request.Request(url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, context=ctx, timeout=30) as r:
            sid = r.headers.get("Mcp-Session-Id")
            if sid:
                session["Mcp-Session-Id"] = sid
            raw = r.read().decode()
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")[:500]
        print(f"  HTTP {e.code} on {payload.get('method')}: {detail}", file=sys.stderr)
        raise
    if notify:
        return None
    for line in raw.splitlines():
        if line.startswith("data: "):
            return json.loads(line[6:])
    return json.loads(raw) if raw.strip() else None


init = rpc({
    "jsonrpc": "2.0", "id": 1, "method": "initialize",
    "params": {
        "protocolVersion": proto,
        "capabilities": {},
        "clientInfo": {"name": "kaed-018-probe", "version": "0"},
    },
})
res = init["result"]
print(f"negotiated protocolVersion: {res['protocolVersion']}", file=sys.stderr)
print(f"server: {res['serverInfo']['name']} {res['serverInfo'].get('version')}", file=sys.stderr)

rpc({"jsonrpc": "2.0", "method": "notifications/initialized"}, notify=True)

tl = rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
tools = tl["result"]["tools"]
print(f"tools/list: {len(tools)} tools -> {', '.join(t['name'] for t in tools)}", file=sys.stderr)
r = tl["result"]
print(f"SEP-2549 on result: ttlMs={r.get('ttlMs')!r} cacheScope={r.get('cacheScope')!r} "
      f"resultType={r.get('resultType')!r}", file=sys.stderr)

if call:
    name, args = call[0], json.loads(call[1]) if len(call) > 1 else {}
    out = rpc({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": name, "arguments": args},
    })
    # Unwrap the MCP envelope so the caller gets the tool's own JSON.
    try:
        body = json.loads(out["result"]["content"][0]["text"])
    except (KeyError, IndexError, ValueError):
        body = out
    json.dump(body, sys.stdout, indent=2)
    print()
