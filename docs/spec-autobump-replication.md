# Replicating `spec-autobump` to the other SDKs and the monorepo

`spec-autobump.yml` in this repo is the **reference implementation** for the
ENG-3563 SDK spec auto-pickup pipeline. It is intentionally landed and reviewed
here first; the other targets replicate the same pattern once it is approved.

## The pipeline, end to end

1. **`nexus-exchange-api`** (`.github/workflows/spec-dispatch.yml`) — on
   `release: published`, sends a `repository_dispatch` (`event_type:
   spec-released`, `client_payload: { "tag": "<new tag>" }`) to each target,
   using `secrets.SDK_DISPATCH_TOKEN` (provisioned by ENG-4149). Targets are a
   matrix; add each one as you land its handler.
2. **Each target** handles `spec-released`, runs oasdiff `old-pin -> new`, bumps
   its pin, and opens a PR. **Non-breaking** arms auto-merge; **breaking** routes
   to a human. A daily `schedule` poll is the self-healing fallback.

## How the classification works (don't change this)

`oasdiff breaking <old-pin-spec> <new-spec> --fail-on ERR`:

- exit **0** → no ERR-level changes → **NON-BREAKING** → arm auto-merge.
- exit **non-zero** → ≥1 ERR-level change → **BREAKING** → human.

This is the same gate the api repo runs as "Classify API changes"
(`.github/workflows/api-diff.yml`), so the SDK and the source agree on what
"breaking" means. WARN/INFO-level changes (e.g. an added optional field, a
removed *optional* response property) are non-breaking.

## Why non-breaking auto-merge is safe to land now

Arming auto-merge does **not** merge. A PR can only merge once **both**:

- the **required status checks** pass (this repo: `drift` + CI `test`), and
- the **ENG-4149 ruleset bypass** is configured so the bot satisfies the
  1-review + code-owner-review rule for pin-bump PRs only.

Until ENG-4149 lands, a non-breaking PR sits green awaiting the bypass — nothing
merges. The drift check is the real safety: an additive spec change needs no SDK
code edits, so drift stays green; it fails only if an *implemented* op was
removed/renamed, which oasdiff would already have classified as breaking.

## Replicating to `-py` / `-cli` / `-mcp`

These repos follow the same `.api-version` + drift pattern as this one. To
replicate:

1. Copy `.github/workflows/spec-autobump.yml`.
2. Port the two helper scripts to the repo's language/idiom (or reuse as-is if
   Python is available in CI):
   - the **pin + "currently targets" line** bump — here `scripts/sync_api_version.py`.
   - the **compat-table row** update — here `scripts/update_compat_table.py`
     (advances the API-spec cell of the table's top row, which by convention is
     the SDK series under active development; language-agnostic — just point it
     at the repo's README).
3. Update the **required checks** referenced in the PR body to that repo's
   drift and test job names.
4. Add the repo to the `matrix.target` list in the api repo's
   `spec-dispatch.yml` (uncomment the TODO entry).
5. Have ENG-4149 scope `SDK_DISPATCH_TOKEN` + the ruleset bypass to the repo.

## Replicating to the monorepo (`nexus`) re-vendor leg

The monorepo **vendors** the spec rather than pinning a release tag, so its
handler differs: instead of bumping `.api-version`, it re-vendors via the
existing script and opens a PR.

1. Add a workflow handling `spec-released` (same dispatch + daily poll triggers).
2. On a new tag, run:

   ```sh
   eng/apps/exchange/scripts/bump-api-spec.sh <tag>
   ```

   which re-vendors the spec for that tag.
3. Run the same oasdiff classification (`old-vendored-spec -> new`,
   `--fail-on ERR`) to decide non-breaking vs breaking.
4. Open the PR with the same auto-merge / human-review split (non-breaking arms
   auto-merge; breaking routes to a human), gated on the monorepo's own required
   checks + the ENG-4149 bypass.
5. Add `nexus-xyz/nexus` to the api repo dispatch matrix.

Do not build these until the reference (`-rs`) is reviewed and merged.
