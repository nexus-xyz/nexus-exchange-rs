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
- The non-blocking **`semver`** CI job (`cargo-semver-checks`) flags
  public-API breaks on the PR. If it's red, either add a `#[deprecated]` alias
  instead, or confirm the break is intended and call it out in the PR — it then
  shows up in the release-plz release PR's "⚠ breaking changes" section.

### Toward 1.0

`0.x` is for iteration. We'll commit to a stable public surface at `1.0`; after
that, breaking changes require a deprecation window and a major bump.

## Cutting a release

release-plz opens and maintains the release PR (version bump + changelog). It does
**not** touch the README, so one step is manual:

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
