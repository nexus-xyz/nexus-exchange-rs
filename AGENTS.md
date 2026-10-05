# Contributing guide — nexus-exchange-rs

The Rust SDK for the Nexus Exchange API.

## Merging

- Don't merge a PR without an approving review — CI passing isn't a substitute.
- Don't merge a PR you didn't author without an approving review **and** the
  author's sign-off. Check the author first
  (`gh pr view <n> --json author,reviewDecision`).
- Re-approval isn't needed for follow-up commits to an already-approved PR.

## Pull requests

- One concern per PR; link its tracking issue (`ENG-XXXX`) in the title.
- Respond to review comments before merging.

## Checks (before pushing)

- `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test` all pass — CI
  enforces these.
- If you changed the public API, regenerate `public-api.txt` with
  `scripts/release_gate/public_api.sh --write` and commit it in the same PR.
  `prepublish-surface` fails on any difference, so a removal shows up in the diff
  a reviewer reads (ENG-18798).

## API contract

- `.api-version` pins a released `nexus-exchange-api` tag; `endpoints.txt` lists
  the operations this SDK implements and is checked against the pinned spec
  (`scripts/check_spec_drift.py`). Update it when you add a typed method.
- Pre-1.0 versioning (release-plz): bump minor on breaking changes, patch on
  features and fixes.
