# Sprint 016 decisions

Per-sprint `D-n`. Cross-sprint decisions live in
`sprints/planning/decisions.md` as `PD-n`.

---

## D-1 — cache metadata is emitted per negotiated revision, not unconditionally

**Considered:** always setting `ttlMs`/`cacheScope` on `tools/list`. It is
one fewer branch, the fields are additive, and most validators tolerate
unknown keys.

**Decided:** set them only when the peer negotiated `2026-07-28` or newer.

The symmetry is the argument. Sprint 015's whole lesson is that an advertised
version is a promise about response *shape*; a server that under-delivers on
that promise breaks a client silently. Over-delivering is the same category of
claim made carelessly — a `2025-11-25` peer is entitled to the shape
`2025-11-25` describes, and "probably tolerated" is not a contract. rmcp
reaches the same conclusion for the neighbouring field: it strips
`resultType: "complete"` for legacy peers precisely because "strict peers may
reject it".

The branch costs one `if` and `RequestContext::protocol_version()`, which is
already correct for both lifecycles — the inline one reports the request's own
`_meta` version, the legacy one the session's negotiated version.

`cacheScope` is `public` rather than `private` because kaed's tool catalog is
compiled in and identical for every author the instance serves. Peer roots
change what a *call* can reach; they never change which tools exist. `ttlMs`
is an hour, which is a conservative reading of the honest bound — the catalog
cannot change without a new build, and a new build is a new process.

## D-2 — the gateway pins the revision it *asks* peers for, below what it serves

**Found while lifting the cap, not predicted:** rmcp 3.1.0's client cannot
drive a conformant `2026-07-28` session. The handshake succeeds — `initialize`
is exempt from the per-request metadata rules — and then the first real call
is rejected by rmcp's **own server**:

```
-32602 Invalid params: request _meta is missing or has malformed required fields:
       io.modelcontextprotocol/protocolVersion, io.modelcontextprotocol/clientCapabilities
```

kaed is not only an MCP server. Since sprint 010 kai is the fleet's gateway,
and every proxied call to `kubs0:*` / `kubsdb:*` goes out through an rmcp
*client* (`fleet.rs`). That client used `ClientInfo::default()`, whose
`protocol_version` is `ProtocolVersion::LATEST`.

Today `LATEST` is `2025-11-25`, so nothing is broken and nothing would have
been caught. **This is sprint 015's D-3 hazard from the other side**: the rmcp
release that promotes `2026-07-28` to `LATEST` would make kai's gateway ask
peers for a revision its own client cannot speak, and every proxied call would
start failing — through a dependency update, with no code change to review.
015 pinned the server's answer for exactly this reason; the client's ask
deserves the same treatment.

**Decided:** `fleet::PEER_PROTOCOL_VERSION`, stated explicitly, currently
`2025-11-25`.

The asymmetry is real and worth naming: kaed *serves* `2026-07-28` and *asks
for* `2025-11-25`. That is not inconsistency — it is two different pieces of
software. kaed's server implements the revision; rmcp's client does not.

The pin is not left to rot. `an_rmcp_client_cannot_yet_drive_2026_07_28` in
`tests/http.rs` asserts the limitation itself, so the day rmcp's client gains
per-request metadata the test fails and says the pin can be raised. A test
that fails on good news is the right shape for a dependency workaround.

## D-3 — 015's clamp middleware is deleted, not neutered

`clamp_protocol_middleware` rewrote an over-new `initialize` at the HTTP
boundary. With `2026-07-28` in the supported list there is nothing left for it
to rewrite except versions kaed has never heard of, and rmcp's own negotiation
already answers those with the ceiling — which is what
`negotiation_answers_the_revision_kaed_implements` still asserts against
`9999-12-31`.

**Considered:** keeping it as a general "never echo an unknown version"
guard. **Decided:** delete it, along with `MAX_REQUEST_BODY_BYTES`, which
existed only to bound its buffering.

015 D-2 was explicit that rewriting a client's request is not something to do
lightly; it was justified by a specific failure it was the only fix for. That
failure is gone. Keeping the middleware would mean buffering every sessionless
POST forever to guard a case rmcp handles — and it is the kind of code that
survives by being hard to argue against rather than by being needed. The
protection it provided now lives where it belongs: in
`SUPPORTED_PROTOCOL_VERSIONS` and the `get_info` pin, both of which 015 D-3
put there and both of which stay.
