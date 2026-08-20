# Sprint 020 decisions

## D-1 — `targets[].txn_id` is how the response distinguishes the three target classes

The proposal asked for an explicit decision on whether `targets[]` should
distinguish "same host, separate txn" from "other host, separate txn" —
different failure modes an agent may care about. Decision: yes, and the
discriminator is **data, not a label**. `RotatedTarget` gains an optional
`txn_id`:

- present and equal to the top-level `txn_id` → written atomically with
  the primary (same-root targets);
- present and different → this host wrote it in its **own** transaction
  (same-host, cross-root) — applied independently, and the id is a journal
  pointer for exactly that write;
- absent → the target lives on another host, which journaled it there.

The host distinction the WI worried about is already derivable — root
names carry their host (`kubsdb:src` vs `kubsdb:datastore`) — so a
separate `via: local|peer` enum would restate what `root` says. What is
*not* derivable from the existing fields is the transaction boundary, and
that is the thing with distinct failure modes. `txn_id` answers it and
hands the agent the journal handle for free.

## D-2 — a same-host cross-root write journals `rotate`, not `transport`

`transport` means "the value left this host" (011 D-5/D-6): the sender
records the departure because the destination's journal is outside its
authority. A cross-root write on the same host never leaves the host's
own journal, so a transport row would be false. Each same-host target
journals a `rotate` secret event under the *target* root with the
target's own `txn_id` — the host's secret stream then has one rotate row
per location rotated on that host, which is what it already means for
same-root `also` targets.

## D-3 — the cross-root target does not join the primary's transaction, and this is schema, not laziness

`txns.root` is a single column (`src/journal.rs`); a transaction is
scoped to one root by design, and the journal's `root` filter, `revert`'s
addressing and the blob attribution all lean on it. Widening it for this
case would reopen 009's model for a bug fix. So the same-host cross-root
target is applied as its own transaction, ordered after the primary's,
and can fail independently — exactly the semantics remote targets already
have, minus the network. `rotate_local`'s refusal of cross-root targets
(`src/secret_tool.rs`) stays: the *server* routes, the local engine still
only ever writes the addressed root.
