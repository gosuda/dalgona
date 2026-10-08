#!/usr/bin/env python3
"""Generate the pinned models.dev price table from the committed snapshot."""

from __future__ import annotations

import json
import math
import re
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import date
from decimal import Decimal
from pathlib import Path
from typing import NoReturn, Union

PRICE_SOURCE: str = "models.dev"
PRICE_SOURCE_URL: str = "https://models.dev/api.json"

SNAPSHOT_PATH: Path = Path("dal/prices/models.dev.json")
FETCHED_AT_PATH: Path = Path("dal/prices/fetched-at.txt")
GENERATED_PATH: Path = Path("dal/crates/dal-provider/src/prices_generated.rs")

USAGE: str = "usage: scripts/update-prices.py [--check]"
STALE_MESSAGE: str = "update-prices: generated price table is stale; run python3 scripts/update-prices.py"
CATALOG_MESSAGE: str = "update-prices: invalid models.dev catalog"
DATE_MESSAGE: str = "update-prices: fetched-at must be one UTC date in YYYY-MM-DD format"
NO_PRICED_MESSAGE: str = "update-prices: no priced models found"

_DATE_PATTERN: re.Pattern[str] = re.compile("([0-9]{4}-[0-9]{2}-[0-9]{2})" + chr(10) + "?")
_MAX_F64: float = sys.float_info.max

JSONValue = Union[bool, int, float, Decimal, str, None, "list[JSONValue]", "dict[str, JSONValue]"]


class UpdatePricesError(Exception):
    """Fatal input error carrying its exact user-facing message."""


@dataclass(frozen=True)
class PriceRow:
    model: str
    input: float | None
    cached_input: float | None
    output: float | None
    reasoning: float | None


def _valid_number(value: JSONValue) -> bool:
    if isinstance(value, bool):
        return False
    if isinstance(value, int):
        return value >= 0 and value <= _MAX_F64 and float(value) == value
    if isinstance(value, float):
        return math.isfinite(value) and value >= 0
    if isinstance(value, Decimal):
        return (
            value.is_finite()
            and value >= 0
            and value <= _MAX_F64
            and (value == 0 or float(value) != 0.0)
        )
    return False


def parse_rate(value: JSONValue) -> float | None:
    if value is None:
        return None
    if not _valid_number(value):
        raise UpdatePricesError(CATALOG_MESSAGE)
    return abs(float(value))


def parse_cost(record: JSONValue) -> tuple[float | None, float | None, float | None, float | None]:
    if not isinstance(record, dict):
        raise UpdatePricesError(CATALOG_MESSAGE)
    cost: JSONValue = record.get("cost")
    if cost is None:
        return None, None, None, None
    if not isinstance(cost, dict):
        raise UpdatePricesError(CATALOG_MESSAGE)
    return (
        parse_rate(cost.get("input")),
        parse_rate(cost.get("cache_read")),
        parse_rate(cost.get("output")),
        parse_rate(cost.get("reasoning")),
    )


def _provider_rows(
    provider: str,
    entry: JSONValue,
    seen: set[str],
) -> tuple[list[PriceRow], list[tuple[str, str]]]:
    if not isinstance(entry, dict):
        raise UpdatePricesError(CATALOG_MESSAGE)
    models: JSONValue = entry.get("models")
    if models is None:
        return [], []
    if not isinstance(models, dict):
        raise UpdatePricesError(CATALOG_MESSAGE)
    rows: list[PriceRow] = []
    temperature_rows: list[tuple[str, str]] = []
    for model, record in models.items():
        if not isinstance(record, dict):
            raise UpdatePricesError(CATALOG_MESSAGE)
        rates = parse_cost(record)
        if "temperature" in record:
            temperature = record["temperature"]
            if not isinstance(temperature, bool):
                raise UpdatePricesError(CATALOG_MESSAGE)
            if temperature:
                key = f"{provider}/{model}"
                if any(0xD800 <= ord(ch) <= 0xDFFF for ch in key):
                    raise UpdatePricesError(CATALOG_MESSAGE)
                temperature_rows.append((provider, model))
        if all(rate is None for rate in rates):
            continue
        key = f"{provider}/{model}"
        if any(0xD800 <= ord(ch) <= 0xDFFF for ch in key) or key in seen:
            raise UpdatePricesError(CATALOG_MESSAGE)
        seen.add(key)
        rows.append(PriceRow(key, *rates))
    return rows, temperature_rows


def _reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON constant {value!r} is not valid JSON")


def load_catalog(path: Path) -> tuple[list[PriceRow], list[tuple[str, str]]]:
    try:
        catalog: JSONValue = json.loads(
            path.read_text(encoding="utf-8"),
            parse_float=Decimal,
            parse_constant=_reject_json_constant,
        )
    except (OSError, ArithmeticError, ValueError) as err:
        raise UpdatePricesError(CATALOG_MESSAGE) from err
    if not isinstance(catalog, dict):
        raise UpdatePricesError(CATALOG_MESSAGE)
    rows: list[PriceRow] = []
    temperature_rows: list[tuple[str, str]] = []
    seen: set[str] = set()
    for provider, entry in catalog.items():
        provider_rows, provider_temperature_rows = _provider_rows(provider, entry, seen)
        rows.extend(provider_rows)
        temperature_rows.extend(provider_temperature_rows)
    if not rows:
        raise UpdatePricesError(NO_PRICED_MESSAGE)
    rows.sort(key=lambda row: row.model)
    temperature_rows.sort()
    return rows, temperature_rows


def load_fetched_at(path: Path) -> str:
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as err:
        raise UpdatePricesError(DATE_MESSAGE) from err
    match = _DATE_PATTERN.fullmatch(text)
    if match is None:
        raise UpdatePricesError(DATE_MESSAGE)
    try:
        fetched = date.fromisoformat(match.group(1))
    except ValueError as err:
        raise UpdatePricesError(DATE_MESSAGE) from err
    return fetched.isoformat()


def f64_literal(value: float) -> str:
    text = format(Decimal(repr(value)), "f")
    if "." not in text:
        text += ".0"
    return text


def rust_string(text: str) -> str:
    slash = chr(92)
    escapes = {
        '"': slash + '"',
        slash: slash + slash,
        chr(10): slash + "n",
        chr(13): slash + "r",
        chr(9): slash + "t",
    }
    parts: list[str] = ['"']
    for ch in text:
        escaped = escapes.get(ch)
        if escaped is not None:
            parts.append(escaped)
        elif ord(ch) < 0x20 or ord(ch) == 0x7F:
            parts.append(f"{slash}u{{{ord(ch):x}}}")
        else:
            parts.append(ch)
    parts.append('"')
    return "".join(parts)


def render_rate(value: float | None) -> str:
    if value is None:
        return "None"
    return f"Some({f64_literal(value)})"


def render(
    fetched_at: str,
    rows: Sequence[PriceRow],
    temperature_rows: Sequence[tuple[str, str]],
) -> str:
    lines: list[str] = [
        f'pub(crate) const PRICE_SOURCE: &str = "{PRICE_SOURCE}";',
        f'pub(crate) const PRICE_SOURCE_URL: &str = "{PRICE_SOURCE_URL}";',
        f'pub(crate) const PRICE_FETCHED_AT: &str = "{fetched_at}";',
        '#[expect(clippy::unreadable_literal, clippy::approx_constant, reason = "generated price literals preserve the snapshot\'s decimal values; 0.318 is an exact price, not FRAC_1_PI")]',
        "pub(crate) const PRICE_ROWS: &[PriceRow] = &[",
    ]
    for row in rows:
        lines.extend(
            [
                "    PriceRow {",
                f"        model: {rust_string(row.model)},",
                f"        input: {render_rate(row.input)},",
                f"        cached_input: {render_rate(row.cached_input)},",
                f"        output: {render_rate(row.output)},",
                f"        reasoning: {render_rate(row.reasoning)},",
                "    },",
            ]
        )
    lines.extend(["];", "pub(crate) const TEMPERATURE_ROWS: &[(&str, &str)] = &["])
    for provider, model in temperature_rows:
        lines.append(f"    ({rust_string(provider)}, {rust_string(model)}),")
    lines.append("];")
    return chr(10).join(lines) + chr(10)


def main(argv: Sequence[str]) -> int:
    check = False
    if len(argv) == 1 and argv[0] == "--check":
        check = True
    elif len(argv) != 0:
        print(USAGE, file=sys.stderr)
        return 2
    root = Path(__file__).resolve().parent.parent
    try:
        rows, temperature_rows = load_catalog(root / SNAPSHOT_PATH)
        fetched_at = load_fetched_at(root / FETCHED_AT_PATH)
    except UpdatePricesError as err:
        print(str(err), file=sys.stderr)
        return 1
    generated = render(fetched_at, rows, temperature_rows).encode("utf-8")
    target = root / GENERATED_PATH
    if check:
        if not target.is_file() or target.read_bytes() != generated:
            print(STALE_MESSAGE, file=sys.stderr)
            return 1
        return 0
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(generated)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
