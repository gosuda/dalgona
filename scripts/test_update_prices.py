#!/usr/bin/env python3
"""Deterministic tests for the price table generator."""

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType


def _load_generator() -> ModuleType:
    spec = importlib.util.spec_from_file_location("update_prices", Path(__file__).with_name("update-prices.py"))
    if spec is None or spec.loader is None:
        raise ImportError("scripts/update-prices.py is not importable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


up = _load_generator()


def tier(size: object, **rates: object) -> dict[str, object]:
    return {"tier": {"type": "context", "size": size}, "input": 1, "output": 2, **rates}


def parse(*tiers: object) -> tuple[up.PriceTier, ...]:
    return up.parse_tiers({"tiers": list(tiers)})


class ParseTiersTest(unittest.TestCase):
    def test_absent_tiers_are_empty(self) -> None:
        self.assertEqual(up.parse_tiers({}), ())
        self.assertEqual(up.parse_tiers({"tiers": None}), ())

    def test_plus_one_boundaries_are_normalized(self) -> None:
        for spelled in sorted(up._CONTEXT_TIER_PLUS_ONE):
            with self.subTest(spelled=spelled):
                self.assertEqual([t.size for t in parse(tier(spelled))], [spelled - 1])

    def test_exactly_five_plus_one_boundaries(self) -> None:
        self.assertEqual(
            up._CONTEXT_TIER_PLUS_ONE,
            frozenset({32_001, 128_001, 200_001, 256_001, 272_001}),
        )

    def test_plus_one_neighbours_are_untouched(self) -> None:
        for spelled in sorted(up._CONTEXT_TIER_PLUS_ONE):
            for size in (spelled - 1, spelled + 1):
                with self.subTest(size=size):
                    self.assertEqual([t.size for t in parse(tier(size))], [size])

    def test_ordinary_thresholds_are_untouched(self) -> None:
        sizes = [0, 1, 100_000, 100_001, 1_000_000]
        self.assertEqual([t.size for t in parse(*(tier(s) for s in sizes))], sizes)

    def test_rates_are_mapped_per_field(self) -> None:
        (parsed,) = parse(tier(10, input=1.5, cache_read=0.25, output=3, reasoning=4, cache_write=5, input_audio=6))
        self.assertEqual(parsed, up.PriceTier(size=10, input=1.5, cached_input=0.25, output=3.0, reasoning=4.0))

    def test_normalization_collision_is_a_duplicate(self) -> None:
        with self.assertRaises(up.UpdatePricesError):
            parse(tier(32_000), tier(32_001))

    def test_duplicate_sizes_are_rejected(self) -> None:
        with self.assertRaises(up.UpdatePricesError):
            parse(tier(10), tier(10))

    def test_unsorted_sizes_are_rejected(self) -> None:
        with self.assertRaises(up.UpdatePricesError):
            parse(tier(20), tier(10))

    def test_unsorted_after_normalization_is_rejected(self) -> None:
        with self.assertRaises(up.UpdatePricesError):
            parse(tier(200_001), tier(150_000))

    def test_malformed_tiers_are_rejected(self) -> None:
        bad: list[object] = [
            [],
            {},
            "tiers",
            [None],
            [{"input": 1}],
            [{"tier": "context", "input": 1}],
            [{"tier": {"type": "context"}, "input": 1}],
            [{"tier": {"type": "context", "size": 1, "extra": 2}, "input": 1}],
            [{"tier": {"type": "audio", "size": 1}, "input": 1}],
            [tier(-1)],
            [tier(True)],
            [tier(1.5)],
            [tier("1")],
            [tier(18_446_744_073_709_551_616)],
            [{"tier": {"type": "context", "size": 1}}],
            [{"tier": {"type": "context", "size": 1}, "cache_write": 1}],
            [tier(1, unknown=1)],
            [tier(1, input=-1)],
            [tier(1, output="2")],
        ]
        for tiers in bad:
            with self.subTest(tiers=tiers):
                with self.assertRaises(up.UpdatePricesError):
                    up.parse_tiers({"tiers": tiers})

    def test_u64_max_size_is_accepted(self) -> None:
        self.assertEqual([t.size for t in parse(tier(18_446_744_073_709_551_615))], [18_446_744_073_709_551_615])


class RenderTest(unittest.TestCase):
    def test_render_emits_every_tier_in_order(self) -> None:
        rows = [
            up.PriceRow("a/plain", 1.0, None, 2.0, None, ()),
            up.PriceRow(
                "b/tiered",
                0.5,
                0.25,
                1.5,
                None,
                (
                    up.PriceTier(31_999 + 1, 0.5, None, 1.5, None),
                    up.PriceTier(200_000, 1.0, 0.5, 3.0, 4.0),
                ),
            ),
        ]
        text = up.render("2026-01-02", rows, [("p", "m")])
        self.assertIn('pub(crate) const PRICE_FETCHED_AT: &str = "2026-01-02";', text)
        self.assertEqual(text.count("TierRow {"), 1)
        self.assertEqual(text.count("PriceTier {"), 2)
        self.assertLess(text.index("size: 32_000,"), text.index("size: 200_000,"))
        self.assertIn('model: "b/tiered",', text.split("PRICE_TIER_ROWS")[1])
        self.assertNotIn("a/plain", text.split("PRICE_TIER_ROWS")[1].split("TEMPERATURE_ROWS")[0])
        self.assertIn("input: Some(0.5),\n                cached_input: None,", text)
        self.assertIn("reasoning: Some(4.0),", text)
        self.assertIn('    ("p", "m"),', text)
        self.assertTrue(text.endswith("];\n"))

    def test_render_is_deterministic(self) -> None:
        rows = [up.PriceRow("m", 1.0, None, None, None, (up.PriceTier(5, 1.0, None, None, None),))]
        self.assertEqual(up.render("2026-01-02", rows, []), up.render("2026-01-02", rows, []))


class LoadCatalogTest(unittest.TestCase):
    def load(self, catalog: object) -> tuple[list[up.PriceRow], list[tuple[str, str]]]:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps(catalog), encoding="utf-8")
            return up.load_catalog(path)

    def test_tiers_flow_from_catalog_to_rows_normalized(self) -> None:
        rows, _ = self.load(
            {"p": {"models": {"m": {"cost": {"input": 1, "output": 2, "tiers": [tier(200_001, input=3, output=4)]}}}}}
        )
        self.assertEqual([r.model for r in rows], ["p/m"])
        self.assertEqual([t.size for t in rows[0].tiers], [200_000])

    def test_malformed_tier_in_catalog_is_fatal(self) -> None:
        with self.assertRaises(up.UpdatePricesError):
            self.load({"p": {"models": {"m": {"cost": {"input": 1, "tiers": [tier(5), tier(5)]}}}}})


class CommittedTableTest(unittest.TestCase):
    def test_generated_table_matches_snapshot(self) -> None:
        self.assertEqual(up.main(["--check"]), 0)


if __name__ == "__main__":
    unittest.main()
