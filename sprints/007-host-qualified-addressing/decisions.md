# Sprint 007 decisions

> Per-sprint `D-n`, distinct from the cross-sprint `PD-n` in
> `../planning/decisions.md`. PD-5 is the decision this sprint implements;
> everything here is a call made *inside* it.

---

## D-1 — The host is configured once, not spelled into every root name

`config.toml` keeps declaring roots by their **local** name (`src`,
`scratch`). The qualifier comes from a single `[server] host` key, and
`ResolvedRoot.name` is the joined `host:local` form.

The alternative — writing `name = "kai:src"` in the config — duplicates the
host on every root and makes the shipped `config.example.toml` per-host,
which it currently is not. One key is also one thing for a future
`install.sh` to set.

**`host` defaults to the short system hostname** (first label of
`/etc/hostname`, falling back to `$HOSTNAME`). That default is what makes
this a zero-touch upgrade: `install.sh` never overwrites an existing config,
so kai and kubs0 gain qualified root names on restart without anyone editing
a file. Startup logs the host it resolved and where it came from — a silent
default that produces a wrong name would be a fresh instance of #930.

If neither source yields a name, kaed **refuses to start** and names
`[server] host` as the fix. Serving unqualified roots "just this once" is
how you end up with two vocabularies.

---

## D-2 — Unqualified root names are rejected, not aliased

`root: "src"` is `not_found`, not a silent alias for `kai:src`. Two spellings
for one root is exactly the second addressing vocabulary the brainstorm's
corollary rules out, and under peer mode `src` would be ambiguous across
hosts anyway.

What makes that affordable is that the **error does the teaching**. Root
lookup failure is now a taxonomy, because the four ways to get it wrong have
four different remedies:

| what was passed | what the error says |
|---|---|
| `src` — a local name that resolves here | root names are host-qualified; did you mean `kai:src`? |
| `kai:nope` | unknown root on this host, with the list |
| `kubs0:src` — a declared, active peer | that host is in the fleet but this instance serves only `kai`; peer routing is korg:1050 |
| `kubsdb:data` — a declared, deferred host | deferred by design, `ref = korg:929`, plus the note |
| `wat:x` | not in this host's declared fleet |

The third and fourth rows are #930 answered on the path a confused agent
actually takes. Discovery tells an agent that already asked; the error
catches the one that assumed.

---

## D-3 — `fleet` is its own block, not pseudo-roots in the `roots` array

#1045 sketches `roots` returning `kubsdb:* → not-deployed-by-design`. A
literal reading would put unaddressable entries in the same array as
addressable ones, and `kubsdb:*` is a name an agent would then try to pass
as `root`.

So the response carries two things: `roots` (what you can address) and
`fleet` (which hosts should run kaed, and what is known about them). Both
arrive in the one call every client already makes, which is what #930's
criterion actually requires — it asks for the answer to be *reachable*, not
for it to be in a particular array.

---

## D-4 — Declared is not observed: `fleet.declared` and per-entry `verified`

This sprint ships against a single instance and probes nothing. Reporting a
config-declared `kubs0: active` as plain `active` would assert something
this instance did not check — a new #930 wearing the fix's clothes.

- `verified: true` only for the entry describing the instance answering the
  call, which is demonstrably serving. Everything else is `verified: false`
  until peer mode fills it in. No shape change needed when it does.
- `fleet.declared` is `false` when the config has no `[peers]` table at all.
  This is the **never-declared** state, made explicit instead of inferred
  from absence. With `declared: true`, a host missing from `hosts` means
  kaed here has no opinion about it; with `declared: false`, absence means
  nothing at all and the agent must not read it as a deferral.

Together with `status`, that keeps PD-5's three states apart:
`deferred` (carries `ref`), `unreachable` (carries `since`), and
never-declared (absence, qualified by `fleet.declared`).

---

## D-5 — `deferred` requires `ref`; `unreachable` requires `since`

Config validation, not convention. A host marked deferred with no pointer to
*why* is the documentation-that-lives-nowhere problem that PD-5 moved the
declaration into `config.toml` to escape; an `unreachable` with no date rots
invisibly and starts lying about the present. Both are startup errors naming
the missing key.

---

## D-6 — Renamed roots orphan journal history: accept and label, don't rewrite

Taking option **(3)** from #1045's comment (korg:1065's third finding).

kai's journal already holds five transactions against a root named `home`
that stopped existing in sprint 002. This sprint renames every remaining
root at once, so after it lands **every** historical row names a root that
`roots` cannot resolve. #1049's `journal`/`diff`/`revert` are built directly
on those rows, one slice later.

- **Not (1) migrate.** Rewriting historical `root` values mutates an
  append-only attributed audit record. R6 exists to prevent exactly that.
- **Not (2) alias.** A compatibility map is permanent, grows with every
  rename, and quietly re-creates the two-vocabulary problem D-2 rejects.
- **(3) accept and label.** The rows are *true*; they describe a
  configuration that no longer exists. `journal` marks any row whose root no
  longer resolves as historical with a structured reason, and `revert`
  refuses such a transaction saying why rather than failing as a bare
  "root not found".

This sprint owes the decision and the contract note, not the code — the
tools that read those rows do not exist yet. What it does owe is a test
pinning that new transactions journal the **qualified** name, so the
boundary #1049 has to detect is real and dated.

---

## D-7 — #1066's fix goes to `list` as well as `search`

#1066 is scoped to `search`, and its own argument is that reporting
`files_searched` "fixes the class rather than the instance". `list` has the
identical trap: the same root-relative `glob`, the same `path` scoping, and
an empty `entries` that reads as "the directory is empty".

Both grow `files_searched` (always present, including zero) and an optional
structured `reason` — `glob_matched_no_files`, `no_files_under_path`,
`all_files_skipped` — whose `hint` is built from the caller's actual
parameters, so it names the fix rather than describing the rule.

Fix option **(3)** from #1066 — making `glob` relative to `path` — is
explicitly **not** taken, per the proposal: it is a silent semantic change to
an existing parameter, and bundling a behaviour change into a rename makes
both harder to review.
