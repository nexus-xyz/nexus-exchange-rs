#!/usr/bin/env python3
"""Regression tests for the enum-member drift invariants (Invariant 5, ENG-5474).

Proves the `spec-drift` gate goes RED on an enum-member delta between the pinned
spec and the SDK — the enforcement gap that let PostOnly (ENG-5058) and the WS
`Channel::Liquidations` variant (ENG-4646) land unmodeled. The SDK side is read
from the real src/types.rs / src/ws/protocol.rs; only the spec side is synthetic,
so the tests are hermetic (no network, no pinned-spec download) yet exercise the
actual parsers against the actual sources.

Run: python3 scripts/test_check_spec_drift.py   (stdlib unittest; no pytest needed)
"""
import contextlib
import io
import os
import sys
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
