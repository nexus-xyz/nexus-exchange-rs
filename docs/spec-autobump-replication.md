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

### Removals: verified, and not always breaking

Measured against `v0.7.2` with oasdiff 1.27.0 (ENG-7961):

| change | oasdiff rule | exit | verdict |
| -- | -- | -- | -- |
| path removed | `api-path-removed-without-deprecation` | 1 | **breaking** → human |
| one method removed from a surviving path | `api-removed-without-deprecation` | 1 | **breaking** → human |
| operation added | — | 0 | non-breaking → arms auto-merge |
| **deprecated + past `x-sunset`, then removed** | — | **0** | **non-breaking → arms auto-merge** |

The first two are the cases the manual-deletion workflow depends on, and they
work. **The fourth is the one to know about:** a removal the api repo deprecates
and sunsets correctly is *not* breaking to oasdiff, so auto-merge is armed and the
drift check is the only thing that objects — invariant 1 goes red because the
removed op is still listed in the manifest.

So oasdiff and the drift check are **not** redundant. Do not treat either as
covering the other.

## Why non-breaking auto-merge is safe to land now

Arming auto-merge does **not** merge. A PR can only merge once **both**:

- the **required status checks** pass (this repo: `spec-drift` + CI `test`), and
- the **ENG-4149 ruleset bypass** is configured so the bot satisfies the
  1-review + code-owner-review rule for pin-bump PRs only.

### "Arms auto-merge" is not "lands unattended"

`allow_auto_merge` is `true` on `-rs` and **`false` on `-py`, `-ts`, `-cli` and
`-mcp`**. It is necessary but not sufficient anywhere:

- Where it is false, `gh pr merge --auto` **fails outright** rather than arming.
  Port the arming step as-is and it silently no-ops (the ENG-7688 shape). Either
  enable the repo setting or have the step probe and say plainly that a human must
  merge.
- Where it is true, the ruleset still requires a review, so until **ENG-4149**
  lands the PR just sits green awaiting the bypass.

Expect to arm auto-merge and see nothing merge. That is the designed state today,
not a bug in the workflow.

### The drift check must not be skippable

The safety argument is "an additive spec change needs no SDK code edits, so drift
stays green; it goes red if an implemented op was removed or renamed." That only
holds if drift actually **runs** on the pin-bump PR, and reports under a name
branch protection can see:

- **Do not put a `paths:` filter on the drift workflow.** A skipped workflow
  reports nothing, and a required check that never runs is satisfied by absence.
  `-rs` originally filtered on the pin plus the three source files the checker
  reads; that worked for pin bumps but had to be maintained in lockstep with the
  checker's inputs, with nothing enforcing it.
- **Do not name it the same as anything else.** `-rs` had two jobs called `drift`
  — CI's pin-lag check and the real manifest verification — so a PR outside the
  path filter showed a green `drift` that had verified nothing. The manifest check
  is now `spec-drift`.

## Replicating to `-py` / `-cli` / `-mcp`

These repos follow the same `.api-version` + drift pattern as this one. To
replicate:

1. Copy `.github/workflows/spec-autobump.yml`.
2. Port the helper script to the repo's language/idiom (or reuse as-is if Python
   is available in CI): the **pin + "currently targets" line** bump — here
   `scripts/sync_api_version.py`.

   Do **not** have the bot touch the README SDK<->spec compatibility table. That
   table records what *released* versions shipped against, so a bare spec release
   changes nothing in it; a row is appended when a release goes out. An earlier
   revision of this pipeline advanced the table's top row on every bump, which
   silently rotted the row's SDK label (it read `0.3.x` while its spec cell had
   marched from `v0.4.0` to `v0.7.1`) because the assumed release-time counterpart
   never existed.
3. Update the **required checks** referenced in the PR body to that repo's
   drift and test job names.
4. Add the repo to the `matrix.target` list in the api repo's
   `spec-dispatch.yml` (uncomment the TODO entry).
5. Have ENG-4149 scope `SDK_DISPATCH_TOKEN` + the ruleset bypass to the repo.
6. Flip `allow_auto_merge` on, or make the arming step report honestly that a
   human must merge. It is `false` on all four today.

**Verification before detection, always.** Adding an autobump to a repo with no
drift check makes things actively worse — it manufactures ungated pin advances at
machine speed. `-py` and `-mcp` are in that state now: land the drift check first.
`-cli` has a both-ways drift check but no self-test, so port
`scripts/test_check_spec_drift.py` alongside it — a checker with no test proving it
goes red when defeated is a checker nobody has reason to trust.

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
