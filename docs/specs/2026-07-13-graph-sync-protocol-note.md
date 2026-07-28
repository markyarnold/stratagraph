# Graph-sync protocol — architecture note

- **Date:** 2026-07-13
- **Status:** Direction note, not a build spec. Written alongside the
  [knowledge-plane design](2026-07-13-knowledge-plane-design.md) so decisions
  made now stay compatible with the shared-graph future. A full design cycle
  happens before any of this is built.
- **Owner decisions locked:** one sync **protocol** with two interchangeable
  homes — a self-hosted, in-estate server for security-sensitive teams, and a
  managed StrataGraph cloud for everyone else (the FSL-reserved hosting
  business is the same code pointed at our endpoint). **Source code never
  leaves the customer estate in any shape**; only graph artifacts move, and
  only where the customer chooses.

## 1. What problem this solves

Team members working on the same repo (or estate) should share **dynamic
graphs**: index once in CI, and every teammate's tools answer from a graph
that matches their checkout — without each machine re-indexing the world, and
without any source code moving.

## 2. Artifact model — commit-anchored graphs

- The unit of sharing is a **graph artifact** keyed by
  `(repo, commit_sha, engine_id)`. Artifacts are immutable; a new commit or a
  new engine build is a new key.
- Artifact content: nodes + edges + coverage metadata. **Never document body
  text** (the knowledge plane stores spans; bodies come from each reader's own
  checkout, which commit-anchoring guarantees they have). **Never source
  text.** This keeps a hosted artifact a *map* of the system, not its
  content — and the map is still confidential (fqns, paths, endpoint and
  table names), which is exactly why the customer chooses where it lives.
- **Delta encoding:** an artifact may be stored as a delta (node/edge
  adds/removes) against its parent commit's artifact, with periodic full
  snapshots. Ties directly into the queued incremental-persist work — one
  storage design serves both.

## 3. Flows

- **Publish (CI):** on push, CI runs `strata index` and publishes the
  artifact for that SHA. The canonical graph is always CI-built — one source
  of truth, no "whose laptop indexed this" ambiguity.
- **Pull (dev):** on checkout/fetch, the client pulls the artifact at the
  nearest ancestor SHA (typically the merge-base) it can find.
- **Overlay (local):** the working tree's uncommitted changes are indexed
  locally as an incremental delta **on top of** the pulled base. Tools answer
  from base+overlay. Offline-first: with no server reachable, everything
  degrades to today's fully-local behaviour.
- **Estates:** per-repo artifacts are pulled independently; estate linking
  runs client-side over the pulled set, exactly as `--workspace` links local
  stores today.

## 4. Homes — one protocol, two deployments

A storage-backend trait with three implementations, chosen per team:

1. **Local directory** (degenerate case; also the test double).
2. **Self-hosted, in-estate** — a small server the customer runs (container
   image), or plain object storage they own (S3-compatible bucket) with
   clients reading/writing directly using credentials the customer controls.
   Nothing reaches us; suits the strictest compliance postures.
3. **Managed StrataGraph cloud** — we operate the same protocol endpoint.
   Tenant-isolated, encrypted at rest. Open question below: client-side
   encryption (we hold ciphertext only) as an option — it constrains
   server-side features to storage/relay, which may be exactly right.

## 5. Obligations this note pins on current work

- **UID stability** across commits for unchanged entities — already the norm;
  the knowledge plane's anchor-based section UIDs were chosen for this.
- **No body text in artifacts** — true by construction after the
  knowledge-plane design.
- **Tantivy index is never synced** — always local-built from the checkout.
- **Engine-id compatibility** — artifacts are keyed by engine build; a client
  on a different engine re-indexes locally rather than mis-reading an
  artifact (honest degradation, no silent schema skew).

## 6. Out of scope for this note

Auth and team identity, billing, the CI integration's packaging (Action vs
plain CLI), artifact retention/GC policy, and the server implementation
itself. Each belongs to the shared-graph design cycle when it starts.

## 7. Open questions for that future cycle

- Client-side encryption for the managed home (custody vs capability
  trade-off).
- Delta granularity vs snapshot cadence (needs the perf program's numbers).
- How `detect_changes`/`blast` compose base+overlay when the base artifact is
  ahead of or behind the working tree's merge-base.
- Whether the self-hosted server and the managed cloud share a binary
  (likely yes: same protocol, different operators).
