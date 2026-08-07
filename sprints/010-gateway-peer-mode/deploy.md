# Sprint 010 — deploy notes

Deploy itself is unchanged: `just publish` → `install.sh --from-store` on
each host (005's store-native path; `sprints/005-store-native-deploy/
deploy.md` is current). What 010 adds is **post-deploy config**, because
`install.sh` never touches an existing `config.toml` and the gateway is
config, not code.

## Post-deploy, per host

Routing is opt-in per instance. The plan for this homelab: **kai is the
gateway** (the brainstorm's reasoning — Linux, tailscale serve already
fronting it). kubs0 gets no peer tokens for now; it stays a plain backend,
reachable directly.

**State discovered 2026-08-07, before ship:** kai's live config *already*
carries `[peers.kubs0]` with `status = "active"` and the real tailnet
`url` — k-homelab's `kaed-service` recipe has converged the fleet block
from `manifests/common.yml kaed_fleet` since its sprint 022. So the only
missing pieces are the `tokens` sub-table and the token file itself. The
intermediate state is graceful by design: a 010 build with url-but-no-
tokens refuses kubs0-root calls with `no_peer_credential` and reports the
peer `probe: skipped` — nothing breaks between deploy and config.

**The constraint that orders the steps:** `kaed-service`'s `kaedconf.py`
(k-homelab; the only clone is `kubs0:~/k-homelab`) manages exactly
`PEER_KEYS = (status, ref, url, note, since)` and **deletes any other
`peers.*` segment it finds** — a hand-added `[peers.kubs0.tokens]` on kai
would be wiped by the next `bin/apply kai kaed-service`, and adding
`tokens:` to `kaed_fleet` instead is refused by its unknown-key check.
So k-homelab moves first:

1. **k-homelab change first — korg #1072** (filed 2026-08-07, and
   korg:1059 `depends_on` it, so the queue shows the gate): teach
   `kaedconf.py` to *preserve* `peers.<host>.tokens` sub-tables it does
   not manage — host-local credential wiring, consistent with the
   recipe's own rule that "this repo never mints or holds kaed tokens".
   Landing it before the kaed deploy means there is never a window where
   an apply can delete live credentials config. The WI carries the full
   defect analysis and test list.
2. **On kai:** create the token file — the `claude` token kubs0's kaed
   accepts, copied `kubs0 → kai` over ssh straight into the file (the
   value never enters an agent context or a repo). `chmod 600`, in
   `~/.config/kaed/peer-tokens/`, which the built-in deny already refuses
   to serve. Then add:

   ```toml
   [peers.kubs0.tokens]
   claude = { token_file = "/home/ken/.config/kaed/peer-tokens/kubs0-claude" }
   ```

3. `systemctl --user restart kaed` — the `[peers]` table is read at
   startup only (reload covers token *values*, not the table).
4. Verify: `kaed check-config` shows `kubs0 … proxies for ["claude"]`;
   `roots` via kai shows kubs0 `verified: true` with a version; `stat
   {root: "kubs0:src", …}` through kai answers; `bin/audit kai
   kaed-service` still reports `ok` (the preserve change holding).
5. Exercise the fallback deliberately: one direct call to kubs0's own URL
   (the existing wiring), so "gateway down ≠ fleet down" stays true in
   practice, not just in docs. cleo needs **no** client change at all —
   its existing kaed-kai connection simply starts seeing the fleet.

The kubsdb entry stays exactly as PD-5 wrote it: `deferred`, `ref
"korg:929"`, no url, no tokens.

## What was actually done

**Deployed 2026-08-07** (`/sprint-ship` Phase 7, from merged main).

- Published `0.1.0-e086181` (`0.1.0 (e086181 2026-08-07)`, the squash
  commit of PR #10) to the package store; `latest` moved to it.
- Installed from the store on kai, then kubs0. All four verification
  checks passed on both hosts: exact version match, `check-config` clean,
  unit active, and an authenticated MCP round trip over each host's real
  tailnet URL returning the same build stamp. Rollback target on both
  hosts: `kaed.prev` = the 009 build; the store's history is the general
  path.
- **Gateway config on kai** (the post-deploy step this file exists for):
  k-homelab #1072 was confirmed landed first (its sprint 023, PR #37 —
  `kaedconf.py` preserves `[peers.<host>.tokens]`). kubs0's `claude`
  token copied over ssh directly into
  `~/.config/kaed/peer-tokens/kubs0-claude` (0600, value never surfaced),
  `[peers.kubs0.tokens]` added to kai's `config.toml`, daemon restarted.
  `check-config`: `kubs0 … proxies for ["claude"]`.
- **Gateway verified live through kai's real URL**: `roots` shows kubs0
  `verified: true` at `0.1.0 (e086181 2026-08-07)` with `kubs0:src` and
  `kubs0:k-homelab` aggregated; `stat {root: "kubs0:src"}` proxied and
  answered; fleet search `*:*` fanned out over all four roots on both
  hosts (755 files searched, per-root `fanout`, kubsdb listed
  `deferred`/korg:929 in `hosts_unavailable`); `journal {root:
  "kubs0:src"}` proxied to kubs0 and answered with its `coverage` block.
- `bin/audit kai kaed-service` → `ok` with the tokens table in place —
  #1072's preservation holding against the live config.
- **Fallback exercised**: the deploy verification's direct MCP round trip
  to kubs0's own URL (with kubs0's token, from kubs0) is the documented
  gateway-down path, and it worked.
- kubs0 deliberately got **no** peer tokens (plain backend, per plan);
  kubsdb untouched (deferred, korg:929).
