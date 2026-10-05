# Contributing

## Compatibility & deprecations

This SDK follows [semver](https://semver.org/). Pre-1.0 (`0.x`), a breaking
change is a **minor** bump — but we still work to minimize and batch them, so
integrators aren't forced through one break at a time.

### Prefer designs that don't need a break

- **Model uncertainty as `Option`/absence, not a guessed concrete value.** If an
  endpoint, URL, or field might not exist or isn't confirmed, return
  `Option<…>` (or don't expose it) rather than shipping a placeholder you'll
  later have to retype. (A return-type change can't be softened with
  deprecation — see below — so get this right up front.)
- **`#[non_exhaustive]`** on public enums, structs, and error types so adding
  variants/fields is non-breaking.
- **Keep struct fields private; expose accessors.** Prefer builder methods /
  optional args for constructors so new parameters don't break call sites.

### When a rename is needed: deprecate, don't remove

Add the new name and keep the old one as a delegating alias for at least one
minor release before removing it:

```rust
#[deprecated(since = "0.3.0", note = "renamed to `ws_base`")]
pub fn ws_url(self) -> Option<&'static str> {
    self.ws_base()
}
```

This only works for a **pure rename** (same signature). A change of return type
or semantics is a genuine break — keeping the old method would preserve the old
(often wrong) behavior, so removal is correct there.

### When a break is unavoidable

- **Batch** breaking changes into a single planned minor bump rather than
  shipping them one-per-PR.
- The **`semver`** CI job (`cargo-semver-checks`) flags public-API breaks on the
  PR, judging only the delta against the base. If it's red, either add a
  `#[deprecated]` alias instead, or **declare** the break so release-plz computes
  the right bump — see [Merging a PR](#merging-a-pr) for how, since an undeclared
  break fails the job. A declared one shows up in the release-plz release PR's
  "⚠ breaking changes" section.

### Toward 1.0

`0.x` is for iteration. We'll commit to a stable public surface at `1.0`; after
that, breaking changes require a deprecation window and a major bump.

## Merging a PR

Squash-and-merge is the only method enabled, and the source branch is deleted on
merge. Two consequences are worth knowing *before* you open the PR, because both
are decided by the title you type.

**The PR title becomes the squash commit's subject, verbatim**
(`squash_merge_commit_title = PR_TITLE`). It therefore has to be a valid
[conventional commit](https://www.conventionalcommits.org/) — `feat(rest): …`,
`fix(docs): …`, `ci: …`. release-plz derives both the changelog section and the
version bump from that subject, so a title it cannot parse files the change under
"Other" and contributes nothing to the bump. Four commits on `main` predate this
and show the symptom.

**Declare a breaking change with `!` before the colon** — `feat(rest)!: …`. The
`semver` job accepts either that or a `BREAKING CHANGE:` footer, but the two are
not equally durable: the squash body is built from the **commit** messages and
never from the PR description, so a footer written only in the PR description is
silently dropped at merge. Prefer `!` in the title; if you use a footer, put it in
a commit body.

This is not hypothetical, and it is why the title is now the source of truth.
[#14](https://github.com/nexus-xyz/nexus-exchange-rs/pull/14) was titled `feat!:`
and CI passed on it, but single-commit PRs used to squash under the *commit*
subject rather than the PR title, and that commit said plain `feat:`. The break
shipped in `v0.3.0` with no `[**breaking**]` marker in the changelog. The released
version was still correct — but only because
[#48](https://github.com/nexus-xyz/nexus-exchange-rs/pull/48) declared its own
break in the same release and carried the minor bump. Had #14 shipped alone, it
would have gone out as a patch.

## Cutting a release

release-plz opens and maintains the release PR (version bump + changelog). Merging
it is the release: the run on that merge publishes to crates.io. No other push
publishes (`release_always = false` in `release-plz.toml`), so a publish that
fails is retried by re-running that run, not by the next merge. The same goes for
a release job that shows **Cancelled**: re-run it. If the publish needs a code fix,
put the fix on a branch named `release-plz-*` (for example `release-plz-fix-ci`):
release-plz publishes only commits associated with a PR from such a branch
([release-plz docs, `release_always`](https://release-plz.dev/docs/config#the-release_always-field)),
and `pre-publish.yml` treats it as a release PR.

**Before merging, the pre-publish checks have to be green on the release PR**
(`.github/workflows/pre-publish.yml`, ENG-18798). release-plz opens the PR with
the default token, so its runs wait for **Approve and run workflows** on the PR.

- `prepublish-surface`: the packed crate's public API equals `public-api.txt`.
- `prepublish-smoke`: the packed crate, built into a clean consumer, lists markets
  on the public testnet. *Testnet unreachable* fails under its own name. It is not
  a pass, so re-run the job once testnet answers.
- `prepublish-verdict`: the proposed version fits the spec change since the last
  published version, graded by the monorepo's classifier
  (`scripts/release_gate/VENDORED.md`). Below 1.0 a breaking change needs a minor
  bump. *Could not classify* fails and needs a person.
- `spec-drift` and `spec-pin`, as on every PR.

The release PR also does **not** touch the README, so one step is manual:

- **Append a row to the README SDK↔spec compatibility table** for the series being
  released, recording the spec in `.api-version` at that point. The table is
  released-version history, so the row is added when the release goes out — not
  when the spec pin moves. Verify against the tags afterwards:

  ```sh
  for t in $(git tag -l 'v*' | sort -V); do echo "$t -> $(git show "$t:.api-version")"; done
  ```

This step used to be assumed automatic. It wasn't, and the table silently
misreported every shipped version from `0.3.x` onward until it was corrected —
so treat a missing row as a release that isn't finished.
