#!/usr/bin/env python3
"""Regression tests for the drift checker's own parsers.

Two enforcement gaps are covered, both of the same shape — a check that reported
green over a real gap:

* **Invariant 5, enum members (ENG-5474).** Proves the `spec-drift` gate goes RED
  on an enum-member delta between the pinned spec and the SDK — the gap that let
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

Either way the tests are hermetic: no network, no pinned-spec download.

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


# Member sets that match the current SDK enums / WS channels exactly.
SDK_SIDE = ["Buy", "Sell"]
SDK_ORDER_TYPE = [
    "Limit",
    "Market",
    "StopLimit",
    "StopMarket",
    "TakeProfitLimit",
    "TakeProfitMarket",
    "TrailingStop",
    "TrailingLimit",
]
SDK_TIF = ["GTC", "IOC", "FOK", "PostOnly"]
SDK_WINDOW = ["day", "week", "month", "all"]
SDK_PUBLIC_CHANNELS = ["trades", "book", "candles"]
SDK_PRIVATE_CHANNELS = ["orders", "fills", "positions", "balances"]


def enum_spec(side=SDK_SIDE, order_type=SDK_ORDER_TYPE, tif=SDK_TIF, window=SDK_WINDOW):
    return {
        "components": {
            "schemas": {
                "OrderRequest": {
                    "properties": {
                        "side": {"enum": list(side)},
                        "order_type": {"enum": list(order_type)},
                        "time_in_force": {"enum": list(tif)},
                    }
                },
                # `window` is composed BY REFERENCE, exactly as the real spec does
                # it (a single-branch `allOf` so a sibling `default` can sit beside
                # the `$ref`) rather than inlining the members. So this fixture
                # also exercises resolve_enum's ref-following: before that existed,
                # this property read as "not an enum" and went unchecked.
                "PortfolioHistory": {
                    "properties": {
                        "window": {
                            "allOf": [
                                {"$ref": "#/components/schemas/PortfolioWindow"}
                            ],
                            "default": "day",
                        }
                    }
                },
                "PortfolioWindow": {"type": "string", "enum": list(window)},
            }
        }
    }


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
        errs = _quiet(csd.check_enums_vs_spec, enum_spec(tif=SDK_TIF + ["GTD"]))
        self.assertGreater(errs, 0)

    def test_sdk_has_member_spec_lacks_fails(self):
        # Bidirectional: SDK would emit a value the API rejects.
        errs = _quiet(
            csd.check_enums_vs_spec, enum_spec(tif=["GTC", "IOC", "FOK"])
        )
        self.assertGreater(errs, 0)

    def test_ahead_of_spec_allowlist_suppresses(self):
        added = {("TimeInForce", "PostOnly")}
        with _patched(csd, "ENUM_MEMBERS_AHEAD_OF_SPEC", added):
            errs = _quiet(
                csd.check_enums_vs_spec, enum_spec(tif=["GTC", "IOC", "FOK"])
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
        errs = _quiet(csd.check_enums_vs_spec, enum_spec(window=SDK_WINDOW + ["quarter"]))
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


if __name__ == "__main__":
    unittest.main(verbosity=2)
