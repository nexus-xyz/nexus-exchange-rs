#!/usr/bin/env python3
"""Regression tests for the drift checker.

The standard every invariant here is held to: **the test must go RED when the
check is defeated.** A test that only proves the checker passes on the known-good
state cannot tell "verified" from "not looking", which is the failure mode the
whole spec-drift design exists to avoid.

Originally only two enforcement gaps were covered, both of the same shape — a
check that reported green over a real gap:

* **Invariant 5, enum members (ENG-5474).** Proves the gate goes RED on an
  enum-member delta between the pinned spec and the SDK — the gap that let
  PostOnly (ENG-5058) and the WS `Channel::Liquidations` variant (ENG-4646) land
  unmodeled. The SDK side is read from the real src/types.rs /
  src/ws/protocol.rs; only the spec side is synthetic.
* **Invariant 2, the REST call parser (ENG-8166).** Proves an endpoint reachable
  ONLY through a cursor paginator is *counted*. The parser was anchored on a
  `self.` receiver, but the paginated readers are called on an owned `Client`
  clone inside a closure, so such an endpoint was silently UNDERCOUNTED — the
  checker would have called it unimplemented. Here the Rust side is synthetic (so
  the contract is pinned regardless of which endpoints happen to be paginated
  today), with a companion class running the same parser over the real
  src/rest.rs.

ENG-7961 audited the suite against the checker and found three invariants whose
comparison logic was never exercised at all — only invariant 5 and invariant 2's
*parser* were covered. Now added:

* **Invariant 1** (`TestInvariant1TargetedVsSpec`) — a manifest entry the pinned
  spec no longer defines. This is the invariant that catches a REMOVED operation,
  and the only one that catches a removal the api repo deprecated and sunset
  properly, because oasdiff classifies that as non-breaking and the autobump
  therefore arms auto-merge.
* **Invariant 2's set comparison** (`TestInvariant2SetEquality`) — real
  bidirectional equality, plus every integrity check on both allowlists,
  including the two ENG-7961 added: a CODE_ONLY_OPS entry the spec has CAUGHT UP
  with, and a NON_REST_TARGETS entry that suppresses nothing.
* **Invariant 3** (`TestInvariant3ModelsVsSpec`) — a model still reading a field
  the spec dropped, the `mark_price` -> `last_trade_price` (PR #48) class.
* **Invariant 4** (`TestInvariant4LoginMessage`) — LOGIN_MESSAGE drift. Those
  bytes are EIP-191 signed at login, so a mismatch rejects every SDK login.

`TestFixtureTracksTheRegistry` guards the suite itself: `enum_spec()` is derived
from ENUM_SCHEMA, so registering an enum can no longer produce a FALSE red.

All tests are hermetic: no network, no pinned-spec download.

Run: python3 scripts/test_check_spec_drift.py   (stdlib unittest; no pytest needed)
"""
import contextlib
import io
import os
import re
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import check_spec_drift as csd  # noqa: E402


def _quiet(fn, *args, **kwargs):
    """Run a check fn, swallowing its stdout; return its error count."""
    with contextlib.redirect_stdout(io.StringIO()):
        return fn(*args, **kwargs)


# Member sets that match the current SDK WS channels exactly.
SDK_PUBLIC_CHANNELS = ["trades", "book", "candles"]
SDK_PRIVATE_CHANNELS = ["orders", "fills", "positions", "balances"]


def _types_src():
    with open(csd.TYPES_RS) as f:
        return f.read()


def sdk_members(rust_name):
    """The wire member set a registered SDK enum actually emits, read from the
    real src/types.rs — the same file, and the same parser, the checker uses."""
    return sorted(csd.parse_enum_members(_types_src(), rust_name))


# Enums the fixture renders BY REFERENCE (a single-branch `allOf` wrapping a
# `$ref`, the idiom that lets a sibling `default` sit beside the ref) instead of
# inlining the `enum` array, exactly as the real spec does for
# `PortfolioHistory.window`. Keeps the fixture exercising resolve_enum's
# ref-following: before that existed, such a property read as "not an enum" and
# went silently unchecked.
REF_COMPOSED = {"PortfolioWindow"}


def enum_spec(**overrides):
    """A synthetic spec whose enum member sets mirror the SDK's exactly.

    Built from `csd.ENUM_SCHEMA` plus the real src/types.rs member sets rather
    than hardcoded, so **registering a new enum does not break these tests**.
    It used to: the fixture declared only `OrderRequest` and `PortfolioWindow`,
    so every enum added to the registry looked like a spec-side omission and
    turned `test_matching_spec_passes` red for a reason that had nothing to do
    with the SDK. That is a false RED — the opposite of the failure this file
    exists to catch, and it cost two PRs a CI failure each (ENG-7961).

    Pass an override keyed by *Rust enum name* to build a spec<->SDK delta, e.g.
    `enum_spec(TimeInForce=[...])`.
    """
    unknown = sorted(set(overrides) - set(csd.ENUM_SCHEMA))
    if unknown:
        raise AssertionError(
            f"enum_spec() override(s) name enum(s) absent from ENUM_SCHEMA: "
            f"{unknown} — fix the test, or register the enum in the checker."
        )

    schemas = {}
    for rust_name, (schema_name, prop) in csd.ENUM_SCHEMA.items():
        members = list(overrides.get(rust_name, sdk_members(rust_name)))
        if rust_name in REF_COMPOSED:
            schemas[rust_name] = {"type": "string", "enum": members}
            node = {
                "allOf": [{"$ref": f"#/components/schemas/{rust_name}"}],
                "default": members[0] if members else None,
            }
        else:
            node = {"enum": members}
        schemas.setdefault(schema_name, {"properties": {}})["properties"][prop] = node
    return {"components": {"schemas": schemas}}


def ws_spec(public=SDK_PUBLIC_CHANNELS, private=SDK_PRIVATE_CHANNELS):
    pub = ", ".join(f"`{c}`" for c in public)
    priv = ", ".join(f"`{c}`" for c in private)
    desc = (
        "WebSocket endpoint.\n\n"
        f"**Public channels** (token required): {pub} — each requires a `market` field.\n\n"
        f"**Per-account channels** (scoped to the wallet): {priv}.\n"
    )
    return {"paths": {"/ws": {"get": {"description": desc}}}}


class TestEnumParser(unittest.TestCase):
    """5a parser: wire-name derivation against the real src/types.rs."""

    @classmethod
    def setUpClass(cls):
        with open(csd.TYPES_RS) as f:
            cls.src = f.read()

    def test_rename_all_uppercase_and_per_variant_rename(self):
        # UPPERCASE rename_all on Gtc/Ioc/Fok + explicit `rename = "PostOnly"`.
        # Also proves comment stripping: PostOnly's doc comment contains
        # parenthesised prose ("(cross the book)") that would otherwise be
        # mis-scanned as a tuple variant.
        self.assertEqual(
            csd.parse_enum_members(self.src, "TimeInForce"),
            {"GTC", "IOC", "FOK", "PostOnly"},
        )

    def test_pascal_case_and_aliases_excluded(self):
        # PascalCase canonical form; the lowercase `alias`es are not wire values.
        self.assertEqual(csd.parse_enum_members(self.src, "Side"), {"Buy", "Sell"})
        self.assertEqual(
            csd.parse_enum_members(self.src, "OrderType"),
            {
                "Limit",
                "Market",
                "StopLimit",
                "StopMarket",
                "TakeProfitLimit",
                "TakeProfitMarket",
                "TrailingStop",
                "TrailingLimit",
            },
        )

    def test_lowercase_rename_all(self):
        self.assertEqual(
            csd.parse_enum_members(self.src, "MarginMode"), {"cross", "isolated"}
        )

    def test_missing_enum_fails_closed(self):
        with self.assertRaises(SystemExit):
            csd.parse_enum_members(self.src, "NoSuchEnum")

    def test_non_unit_variant_fails_closed(self):
        # OrderResult is a data-carrying (struct-variant) enum: the string-enum
        # check must refuse it loudly rather than mis-parse its fields.
        with self.assertRaises(SystemExit):
            csd.parse_enum_members(self.src, "OrderResult")


class TestEnumsVsSpec(unittest.TestCase):
    """5a: src/types.rs serde enums vs spec property `enum` arrays."""

    def test_matching_spec_passes(self):
        self.assertEqual(_quiet(csd.check_enums_vs_spec, enum_spec()), 0)

    def test_spec_adds_member_sdk_lacks_fails(self):
        # The PostOnly/ENG-5058 class: spec gains a member the SDK cannot express.
        errs = _quiet(csd.check_enums_vs_spec, enum_spec(TimeInForce=sdk_members("TimeInForce") + ["GTD"]))
        self.assertGreater(errs, 0)

    def test_sdk_has_member_spec_lacks_fails(self):
        # Bidirectional: SDK would emit a value the API rejects.
        errs = _quiet(
            csd.check_enums_vs_spec, enum_spec(TimeInForce=["GTC", "IOC", "FOK"])
        )
        self.assertGreater(errs, 0)

    def test_ahead_of_spec_allowlist_suppresses(self):
        added = {("TimeInForce", "PostOnly")}
        with _patched(csd, "ENUM_MEMBERS_AHEAD_OF_SPEC", added):
            errs = _quiet(
                csd.check_enums_vs_spec, enum_spec(TimeInForce=["GTC", "IOC", "FOK"])
            )
        self.assertEqual(errs, 0)

    def test_stale_allowlist_entry_fails(self):
        # Member is allowlisted as ahead-of-spec but the spec now defines it.
        added = {("TimeInForce", "PostOnly")}
        with _patched(csd, "ENUM_MEMBERS_AHEAD_OF_SPEC", added):
            errs = _quiet(csd.check_enums_vs_spec, enum_spec())  # spec has PostOnly
        self.assertGreater(errs, 0)

    def test_renamed_property_fails_closed(self):
        spec = enum_spec()
        del spec["components"]["schemas"]["OrderRequest"]["properties"]["time_in_force"]
        self.assertGreater(_quiet(csd.check_enums_vs_spec, spec), 0)

    def test_ref_composed_enum_member_delta_fails(self):
        # PortfolioWindow reaches its members through `allOf`/`$ref`. Prove the
        # invariant actually bites there too: a spec-side member the SDK cannot
        # express must fail, not pass because the property looked non-enum.
        errs = _quiet(csd.check_enums_vs_spec, enum_spec(PortfolioWindow=sdk_members("PortfolioWindow") + ["quarter"]))
        self.assertGreater(errs, 0)

    def test_ref_composed_enum_unresolvable_fails_closed(self):
        # If the referenced schema goes missing, the ref no longer resolves; that
        # must be a loud failure rather than a silently skipped check.
        spec = enum_spec()
        del spec["components"]["schemas"]["PortfolioWindow"]
        self.assertGreater(_quiet(csd.check_enums_vs_spec, spec), 0)


class TestResolveEnum(unittest.TestCase):
    """resolve_enum: how a property reaches its `enum` array."""

    SCHEMAS = {
        "Window": {"enum": ["day", "all"]},
        "Alias": {"$ref": "#/components/schemas/Window"},
        "SelfRef": {"$ref": "#/components/schemas/SelfRef"},
        "Ping": {"$ref": "#/components/schemas/Pong"},
        "Pong": {"$ref": "#/components/schemas/Ping"},
    }

    def _resolve(self, node):
        return csd.resolve_enum(self.SCHEMAS, node)

    def test_inline(self):
        self.assertEqual(self._resolve({"enum": ["a", "b"]}), ["a", "b"])

    def test_direct_ref(self):
        self.assertEqual(
            self._resolve({"$ref": "#/components/schemas/Window"}), ["day", "all"]
        )

    def test_all_of_wrapper(self):
        # The real spec shape: a `default` alongside a single-branch `allOf`.
        node = {"allOf": [{"$ref": "#/components/schemas/Window"}], "default": "day"}
        self.assertEqual(self._resolve(node), ["day", "all"])

    def test_chained_ref(self):
        self.assertEqual(
            self._resolve({"$ref": "#/components/schemas/Alias"}), ["day", "all"]
        )

    def test_multi_branch_all_of_unresolved(self):
        # A real intersection: no basis to pick one member set, so don't guess.
        node = {"allOf": [{"$ref": "#/components/schemas/Window"}, {"enum": ["x"]}]}
        self.assertIsNone(self._resolve(node))

    def test_self_referential_ref_terminates(self):
        self.assertIsNone(self._resolve({"$ref": "#/components/schemas/SelfRef"}))

    def test_mutually_recursive_refs_terminate(self):
        self.assertIsNone(self._resolve({"$ref": "#/components/schemas/Ping"}))

    def test_missing_target_and_non_schema_ref(self):
        self.assertIsNone(self._resolve({"$ref": "#/components/schemas/Nope"}))
        self.assertIsNone(self._resolve({"$ref": "other.json#/Window"}))

    def test_no_enum_anywhere(self):
        self.assertIsNone(self._resolve({"type": "string"}))
        self.assertIsNone(self._resolve(None))
        # An empty `enum` is treated as "no members", same as absent.
        self.assertIsNone(self._resolve({"enum": []}))


class TestWsChannelParser(unittest.TestCase):
    """5b parser: Channel wire names from the real src/ws/protocol.rs."""

    def test_channel_names(self):
        self.assertEqual(
            csd.parse_ws_channel_names(),
            {"trades", "book", "candles", "orders", "fills", "positions", "balances"},
        )


class TestWsChannelsVsSpec(unittest.TestCase):
    """5b: WS `Channel` enum vs the channels documented in `GET /ws`."""

    def test_matching_spec_passes(self):
        self.assertEqual(_quiet(csd.check_ws_channels_vs_spec, ws_spec()), 0)

    def test_spec_adds_channel_sdk_lacks_fails(self):
        # The Liquidations/ENG-4646 class: spec documents a channel the SDK's
        # Channel enum can't subscribe to.
        errs = _quiet(
            csd.check_ws_channels_vs_spec,
            ws_spec(private=SDK_PRIVATE_CHANNELS + ["liquidations"]),
        )
        self.assertGreater(errs, 0)

    def test_sdk_has_channel_spec_lacks_fails(self):
        errs = _quiet(
            csd.check_ws_channels_vs_spec, ws_spec(private=["orders", "fills"])
        )
        self.assertGreater(errs, 0)

    def test_market_field_not_treated_as_channel(self):
        # `market` appears (backticked) in the public line's trailing prose; it
        # must not leak into the channel set (which would make the check pass a
        # spec that is really missing a channel, or spuriously fail).
        self.assertNotIn("market", csd.spec_ws_channels(ws_spec()))

    def test_reworded_description_fails_closed(self):
        bad = {"paths": {"/ws": {"get": {"description": "no channel markers here"}}}}
        with self.assertRaises(SystemExit):
            csd.spec_ws_channels(bad)

    def test_missing_ws_path_fails_closed(self):
        with self.assertRaises(SystemExit):
            csd.spec_ws_channels({"paths": {}})


class TestRestCallParser(unittest.TestCase):
    """Invariant 2's code parser (ENG-8166).

    The parser used to be anchored on a `self.` receiver, so the cursor-paginated
    readers — called on an owned `Client` clone inside a paginator closure — were
    invisible to it, and an endpoint reachable ONLY through a paginator was
    silently UNDERCOUNTED. These fixtures are synthetic Rust so they pin the
    parser's contract directly rather than depending on which endpoints happen to
    be paginated today; `TestRestCallParserAgainstRealSource` below covers the real
    src/rest.rs.
    """

    def _ops(self, source):
        """Run `implemented_ops` over a synthetic src/rest.rs, quietly."""
        with tempfile.NamedTemporaryFile("w", suffix=".rs", delete=False) as fh:
            fh.write(source)
            name = fh.name
        self.addCleanup(os.unlink, name)
        with contextlib.redirect_stdout(io.StringIO()):
            return csd.implemented_ops(path=name)

    # A paginator-only endpoint: reachable through `signed_get_page` on a cloned
    # `Client` and NOTHING else. Formatted exactly as rustfmt wraps it, with the
    # receiver on its own line and a turbofish naming the response type.
    PAGINATOR_ONLY = """
    pub fn fetch_widgets_paginated(&self) -> Paginator<Widget> {
        let client = self.clone();
        Paginator::new(move |req: PageRequest| {
            let client = client.clone();
            async move {
                let (items, next) = client
                    .signed_get_page::<Vec<Widget>>("/api/v1/widgets", &page_query(&req))
                    .await?;
                Ok(Page::new(items, next))
            }
        })
    }
"""

    def test_paginator_only_endpoint_is_counted(self):
        """The regression itself: this endpoint has no `self.<helper>(` call site
        anywhere, so before ENG-8166 invariant 2 counted zero ops for it and would
        have reported `/api/v1/widgets` as unimplemented."""
        self.assertNotIn(
            "self.signed_get",
            self.PAGINATOR_ONLY,
            "fixture must reach the endpoint ONLY through the paginator",
        )
        self.assertIn(("GET", "/api/v1/widgets"), self._ops(self.PAGINATOR_ONLY))

    def test_old_self_anchored_pattern_would_have_missed_it(self):
        """Pins *why* this needed fixing. The pre-ENG-8166 pattern is rebuilt here
        verbatim — a literal `self.` receiver, no turbofish — and finds **zero**
        call sites in the very source the current parser reads correctly. On the
        real src/rest.rs that undercount was masked, because both paginated paths
        were also reached by a plain `self.get` / `self.signed_get`; add a
        paginator-only endpoint and the checker reports green over a gap.
        """
        old = re.compile(r"self\.(" + csd._HELPER_ALT + r")\s*\(\s*")
        self.assertEqual(len(old.findall(self.PAGINATOR_ONLY)), 0)
        # The current parser finds it.
        self.assertEqual(
            len(csd._CALL_SITE_RE.findall(self.PAGINATOR_ONLY)),
            1,
            "the current parser must see exactly the one paginated call site",
        )

    def test_wrapped_receiver_and_nested_generics(self):
        """rustfmt puts the receiver on its own line and the turbofish carries
        nested angle brackets; both have to survive the regex."""
        src = """
        let (items, next) = client
            .get_page::<Vec<HashMap<String, Widget>>>("/api/v1/widgets/nested", &q, COST)
            .await?;
        """
        self.assertIn(("GET", "/api/v1/widgets/nested"), self._ops(src))

    def test_self_receiver_still_counted(self):
        """No regression on the ordinary methods, including `&format!()` paths."""
        src = """
        self.signed_get("/api/v1/plain", &[]).await
        self.get(&format!("/api/v1/markets/{id}/ticker"), &[], COST_DEFAULT)
        """
        ops = self._ops(src)
        self.assertIn(("GET", "/api/v1/plain"), ops)
        self.assertIn(("GET", "/api/v1/markets/{}/ticker"), ops)

    def test_non_inline_path_on_a_paginator_call_is_rejected(self):
        """The inline-literal convention now covers the paginated helpers too — a
        path built into a local first must abort, not be silently dropped."""
        src = """
        let path = format!("/api/v1/widgets/{id}");
        self.signed_get("/api/v1/plain", &[]).await
        let (items, next) = client
            .signed_get_page::<Vec<Widget>>(&path, &page_query(&req))
            .await?;
        """
        with self.assertRaises(SystemExit):
            self._ops(src)

    def test_unknown_receiver_is_not_counted(self):
        """The receiver set is an explicit alternation, not `\\w+\\.`: an unrelated
        receiver must not be mistaken for a REST helper call. Paired with the
        loud-failure paths above, that keeps a NEW receiver a deliberate edit
        rather than something the parser guesses at.

        The suffix cases are the interesting ones. `_RECEIVER_ALT` is anchored with
        a leading `\\b`, without which the alternation degrades into a *suffix*
        match and silently counts every receiver merely ending in `self`/`client`
        — an allowlist in name only. A receiver sharing no suffix (the original
        `some_other_thing`) passes either way, so it cannot pin the property on
        its own."""
        src = """
        self.signed_get("/api/v1/plain", &[]).await
        some_other_thing.signed_get("/api/v1/not-ours", &[]).await
        some_client.signed_get("/api/v1/suffix-client", &[]).await
        http_client.signed_get("/api/v1/prefixed-client", &[]).await
        myself.signed_get("/api/v1/suffix-self", &[]).await
        """
        ops = self._ops(src)
        self.assertIn(("GET", "/api/v1/plain"), ops)
        self.assertNotIn(("GET", "/api/v1/not-ours"), ops)
        # Receivers that merely END in an allowed name are not allowed names.
        self.assertNotIn(("GET", "/api/v1/suffix-client"), ops)
        self.assertNotIn(("GET", "/api/v1/prefixed-client"), ops)
        self.assertNotIn(("GET", "/api/v1/suffix-self"), ops)

    def test_bare_allowed_receivers_still_match_at_token_start(self):
        """The `\\b` must not over-correct: both allowed receivers have to keep
        matching at a token start, including after punctuation that is not a word
        character (`(`, `&`, `=`), which is where a naive anchor like `(?<=\\s)`
        would silently drop sites."""
        src = """
        self.get("/api/v1/a", &[]).await
        client.get("/api/v1/b", &[]).await
        let (items, next) = client
            .signed_get_page::<Vec<Fill>>("/api/v1/c", &page_query(&req))
            .await?;
        """
        ops = self._ops(src)
        for path in ("/api/v1/a", "/api/v1/b", "/api/v1/c"):
            self.assertIn(("GET", path), ops)

    def test_helper_and_call_site_regexes_agree(self):
        """The two regexes must match the same sites, or the count-agreement assert
        inside `implemented_ops` fires. Cheap guard against them drifting apart."""
        src = self.PAGINATOR_ONLY + """
        self.signed_get("/api/v1/plain", &[]).await
        """
        self.assertEqual(
            sum(1 for _ in csd._CALL_SITE_RE.finditer(src)),
            sum(1 for _ in csd._CALL_RE.finditer(src)),
        )


class TestRestCallParserAgainstRealSource(unittest.TestCase):
    """The same parser against the real src/rest.rs, so the fixtures above cannot
    pass while the actual source has drifted out from under them."""

    def test_paginated_helpers_are_reached_in_the_real_source(self):
        src = open(csd.REST_RS).read()
        receivers = {
            (m.group(1), m.group(2))
            for m in csd._CALL_SITE_RE.finditer(src)
        }
        paginated = {r for r in receivers if r[1].endswith("_page")}
        self.assertTrue(
            paginated,
            "no paginated helper call site found in src/rest.rs; if the paginator "
            "wiring moved, update HELPER_METHOD / _RECEIVER_ALT",
        )
        # They are called on a cloned `Client`, never on `self` — the whole reason
        # the receiver alternation exists.
        self.assertEqual({r[0] for r in paginated}, {"client"})

    def test_real_source_has_no_non_inline_paths(self):
        with contextlib.redirect_stdout(io.StringIO()):
            ops = csd.implemented_ops()
        self.assertTrue(ops)


@contextlib.contextmanager
def _patched(module, name, value):
    """Temporarily set module.<name> = value, restoring the original after."""
    original = getattr(module, name)
    setattr(module, name, value)
    try:
        yield
    finally:
        setattr(module, name, original)


@contextlib.contextmanager
def _endpoints_file(lines):
    """Write a temporary endpoints.txt and yield its path."""
    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as f:
        f.write("\n".join(lines) + "\n")
        path = f.name
    try:
        yield path
    finally:
        os.unlink(path)


class TestFixtureTracksTheRegistry(unittest.TestCase):
    """The fixture must not need hand-editing when the registry grows.

    ENUM_SCHEMA is the checker's list of policed enums; `enum_spec()` builds the
    synthetic spec side. When those two were maintained separately, registering
    an enum turned `TestEnumsVsSpec` red with a spec-side omission that said
    nothing about the SDK — a false RED, which is worse than a missed check
    because it trains people to treat this suite's failures as noise (ENG-7961).
    """

    def test_registering_an_enum_does_not_break_the_matching_spec(self):
        # `MarginMode` is a real lowercase serde enum in types.rs that is not in
        # ENUM_SCHEMA today, so it stands in for "the next enum someone registers".
        registry = dict(csd.ENUM_SCHEMA)
        registry["MarginMode"] = ("AccountState", "margin_mode")
        with _patched(csd, "ENUM_SCHEMA", registry):
            self.assertEqual(_quiet(csd.check_enums_vs_spec, enum_spec()), 0)

    def test_a_newly_registered_enum_is_actually_policed(self):
        # The flip side: auto-covering the new enum must not mean ignoring it.
        registry = dict(csd.ENUM_SCHEMA)
        registry["MarginMode"] = ("AccountState", "margin_mode")
        with _patched(csd, "ENUM_SCHEMA", registry):
            errs = _quiet(csd.check_enums_vs_spec, enum_spec(MarginMode=["cross"]))
        self.assertGreater(errs, 0)

    def test_an_override_for_an_unregistered_enum_is_rejected(self):
        # Guards against a typo'd override silently testing nothing.
        with self.assertRaises(AssertionError):
            enum_spec(NoSuchEnum=["a"])


class TestInvariant1TargetedVsSpec(unittest.TestCase):
    """Invariant 1: every endpoints.txt entry must exist in the pinned spec.

    This is the invariant that catches a *removed* operation, and it is the only
    thing that catches one when the removal was properly deprecated + sunset —
    oasdiff classifies that as NON-breaking, so the autobump would arm auto-merge
    (ENG-7961). Nothing here had a test before.
    """

    SPEC = {
        "paths": {
            "/markets": {"get": {}},
            "/orders/{order_id}": {"get": {}, "delete": {}},
        }
    }

    def test_matching_manifest_passes(self):
        available = csd.spec_ops(self.SPEC)
        with _endpoints_file(["GET /markets", "GET /orders/{order_id}"]) as p:
            targeted = csd.load_targeted(p)
        self.assertEqual([op for op in targeted if op not in available], [])

    def test_removed_operation_is_detected(self):
        available = csd.spec_ops(self.SPEC)
        with _endpoints_file(["GET /markets", "GET /gone"]) as p:
            targeted = csd.load_targeted(p)
        self.assertEqual(
            [op for op in targeted if op not in available], [("GET", "/gone")]
        )

    def test_method_removed_from_a_surviving_path_is_detected(self):
        # A subtler removal than a whole path: the path stays, one verb goes.
        available = csd.spec_ops(self.SPEC)
        with _endpoints_file(["POST /orders/{order_id}"]) as p:
            targeted = csd.load_targeted(p)
        self.assertEqual(
            [op for op in targeted if op not in available],
            [("POST", "/orders/{order_id}")],
        )

    def test_placeholder_rename_is_detected_invariant_1_is_exact(self):
        # Invariant 1 compares raw paths, so a spec-side placeholder RENAME goes
        # red even though invariant 2 would treat it as positionally equal. That
        # asymmetry is deliberate (fail loud, then a human edits the manifest) —
        # pinned here so nobody "fixes" it into a silent pass.
        available = csd.spec_ops(self.SPEC)
        with _endpoints_file(["GET /orders/{id}"]) as p:
            targeted = csd.load_targeted(p)
        self.assertEqual(
            [op for op in targeted if op not in available], [("GET", "/orders/{id}")]
        )
        self.assertEqual(
            csd.normalize_path("/orders/{id}"), csd.normalize_path("/orders/{order_id}")
        )

    def test_malformed_line_fails_closed(self):
        with _endpoints_file(["GET"]) as p:
            with self.assertRaises(SystemExit):
                csd.load_targeted(p)

    def test_duplicate_entry_fails_closed(self):
        with _endpoints_file(["GET /markets", "GET /markets"]) as p:
            with self.assertRaises(SystemExit):
                csd.load_targeted(p)


class TestInvariant2SetEquality(unittest.TestCase):
    """Invariant 2: implemented ops == endpoints.txt, modulo the allowlists.

    `implemented_ops` (the parser) was already covered; `check_code_vs_targets`
    (the set comparison, and both allowlists' integrity checks) was not.
    """

    IMPL = {("GET", "/markets"), ("POST", "/orders")}
    SPEC = {"paths": {"/markets": {"get": {}}, "/orders": {"post": {}}}}

    @contextlib.contextmanager
    def _world(self, impl=None, code_only=frozenset(), non_rest=frozenset()):
        with _patched(csd, "implemented_ops", lambda: set(
            self.IMPL if impl is None else impl
        )):
            with _patched(csd, "CODE_ONLY_OPS", set(code_only)):
                with _patched(csd, "NON_REST_TARGETS", set(non_rest)):
                    yield

    def _run(self, targeted, available=None):
        return _quiet(
            csd.check_code_vs_targets,
            targeted,
            csd.spec_ops(self.SPEC) if available is None else available,
        )

    def test_exact_match_passes(self):
        with self._world():
            self.assertEqual(self._run([("GET", "/markets"), ("POST", "/orders")]), 0)

    def test_implemented_but_unlisted_fails(self):
        # Subset-only checking would miss this: the manifest under-counts.
        with self._world():
            self.assertGreater(self._run([("GET", "/markets")]), 0)

    def test_listed_but_unimplemented_fails(self):
        with self._world():
            errs = self._run(
                [("GET", "/markets"), ("POST", "/orders"), ("GET", "/ghost")]
            )
        self.assertGreater(errs, 0)

    def test_code_only_ops_suppresses_an_unlisted_op(self):
        # The allowlisted op must be genuinely AHEAD of the spec — `/ahead` is
        # absent from SPEC. (Using a spec-declared op here instead correctly
        # trips the landed-entry check below, which is the point of that check.)
        ahead = ("POST", "/ahead")
        with self._world(impl=self.IMPL | {ahead}, code_only={ahead}):
            self.assertEqual(self._run([("GET", "/markets"), ("POST", "/orders")]), 0)

    def test_non_rest_targets_suppresses_an_unimplemented_entry(self):
        with self._world(non_rest={("GET", "/ws")}):
            errs = self._run(
                [("GET", "/markets"), ("POST", "/orders"), ("GET", "/ws")]
            )
        self.assertEqual(errs, 0)

    def test_stale_code_only_entry_fails(self):
        # Allowlisted but no longer implemented anywhere.
        with self._world(code_only={("DELETE", "/vanished")}):
            self.assertGreater(
                self._run([("GET", "/markets"), ("POST", "/orders")]), 0
            )

    def test_code_only_entry_the_spec_now_defines_fails(self):
        # The damaging rot direction (ENG-7961): the op is deliberately kept OUT
        # of endpoints.txt, so once the spec declares it, invariant 1 stops
        # checking that its path exists and coverage silently understates.
        with self._world(code_only={("POST", "/orders")}):
            self.assertGreater(self._run([("GET", "/markets")]), 0)

    def test_stale_non_rest_entry_fails(self):
        # Allowlisted as "targeted without a REST helper" but not targeted at all.
        with self._world(non_rest={("GET", "/ws")}):
            self.assertGreater(
                self._run([("GET", "/markets"), ("POST", "/orders")]), 0
            )


class TestInvariant3ModelsVsSpec(unittest.TestCase):
    """Invariant 3: a model must not read a wire field the spec dropped.

    SDK side is the real src/types.rs; only the spec side is synthetic — same
    framing as the enum tests.
    """

    RUST = "RateLimitStatus"
    SCHEMA = "RateLimitStatus"

    def _spec(self, fields):
        return {
            "components": {
                "schemas": {self.SCHEMA: {"properties": {f: {} for f in fields}}}
            }
        }

    def _sdk_fields(self):
        return csd.parse_model_fields(_types_src(), self.RUST)

    def _run(self, spec):
        with _patched(csd, "MODEL_SCHEMA", {self.RUST: self.SCHEMA}):
            return _quiet(csd.check_models_vs_spec, spec)

    def test_matching_spec_passes(self):
        self.assertEqual(self._run(self._spec(self._sdk_fields())), 0)

    def test_dropped_field_fails(self):
        # The mark_price -> last_trade_price (PR #48) class: the spec drops a
        # field the struct still deserializes, so it goes quietly None at runtime.
        fields = sorted(self._sdk_fields())
        self.assertTrue(fields, "expected the model to have parsed fields")
        self.assertGreater(self._run(self._spec(fields[1:])), 0)

    def test_renamed_schema_fails_closed(self):
        with _patched(csd, "MODEL_SCHEMA", {self.RUST: "NoSuchSchema"}):
            errs = _quiet(csd.check_models_vs_spec, self._spec(self._sdk_fields()))
        self.assertGreater(errs, 0)

    def test_schema_without_inline_properties_fails_closed(self):
        # Shape changed to $ref/allOf composition: the field comparison can no
        # longer run, so it must say so rather than silently pass.
        spec = {"components": {"schemas": {self.SCHEMA: {"allOf": []}}}}
        self.assertGreater(self._run(spec), 0)

    def test_ahead_of_spec_allowlist_suppresses_and_goes_stale(self):
        fields = sorted(self._sdk_fields())
        ahead = {(self.RUST, fields[0])}
        with _patched(csd, "MODEL_FIELDS_AHEAD_OF_SPEC", ahead):
            # Suppressed while the spec lacks it...
            self.assertEqual(self._run(self._spec(fields[1:])), 0)
            # ...and flagged as stale once the spec defines it.
            self.assertGreater(self._run(self._spec(fields)), 0)


class TestInvariant4LoginMessage(unittest.TestCase):
    """Invariant 4: LOGIN_MESSAGE must equal the spec's canonical value.

    These bytes are EIP-191 signed at login, so a mismatch rejects every SDK
    login. Read from the real src/rest.rs; the spec side is synthetic.
    """

    def _spec(self, message):
        """The primary shape: the POST /auth/login request-body example."""
        return {
            "paths": {
                "/auth/login": {
                    "post": {
                        "requestBody": {
                            "content": {
                                "application/json": {
                                    "example": {"message": message}
                                }
                            }
                        }
                    }
                }
            }
        }

    def _fallback_spec(self, message):
        """The documented fallback: LoginRequest.message's description."""
        return {
            "components": {
                "schemas": {
                    "LoginRequest": {
                        "properties": {
                            "message": {
                                "description": f'Must be exactly: "{message}"'
                            }
                        }
                    }
                }
            }
        }

    def test_matching_message_passes(self):
        sdk = csd.sdk_login_message()
        self.assertEqual(_quiet(csd.check_login_message, self._spec(sdk)), 0)

    def test_drifted_message_fails(self):
        sdk = csd.sdk_login_message()
        errs = _quiet(csd.check_login_message, self._spec(sdk + " v2"))
        self.assertGreater(errs, 0)

    def test_fallback_source_is_used_and_also_bites(self):
        sdk = csd.sdk_login_message()
        self.assertEqual(_quiet(csd.check_login_message, self._fallback_spec(sdk)), 0)
        errs = _quiet(csd.check_login_message, self._fallback_spec(sdk + " v2"))
        self.assertGreater(errs, 0)

    def test_missing_from_spec_fails_closed(self):
        # Neither source present: must abort, not silently treat as matching.
        with self.assertRaises(SystemExit):
            _quiet(csd.check_login_message, {"paths": {}})

    def test_sdk_constant_is_a_plain_literal(self):
        # Fails closed if the constant is renamed or stops being a plain literal.
        self.assertIsInstance(csd.sdk_login_message(), str)
        self.assertTrue(csd.sdk_login_message())


if __name__ == "__main__":
    unittest.main(verbosity=2)
