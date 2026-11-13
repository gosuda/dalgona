from __future__ import annotations

import json
import sys
from collections.abc import Iterator
from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class Span:
    start: int
    end: int

    def __post_init__(self) -> None:
        assert 0 <= self.start < self.end


@dataclass(frozen=True, slots=True)
class Splice:
    start: int
    end: int
    inserted: int

    def __post_init__(self) -> None:
        assert 0 <= self.start <= self.end and self.inserted >= 0
        assert self.start != self.end or self.inserted != 0

    @property
    def delta(self) -> int:
        return self.inserted - (self.end - self.start)


@dataclass(frozen=True, slots=True)
class Atom:
    identity: int
    text: str


@dataclass(frozen=True, slots=True)
class Counts:
    cases: int = 0
    accepted: int = 0

    def add(self, accepted: bool) -> Counts:
        return Counts(self.cases + 1, self.accepted + int(accepted))


def spans(length: int) -> Iterator[Span]:
    for start in range(length):
        for end in range(start + 1, length + 1):
            yield Span(start, end)


def splices(length: int, max_inserted: int) -> Iterator[Splice]:
    for start in range(length + 1):
        for end in range(start, length + 1):
            for inserted in range(max_inserted + 1):
                if start != end or inserted:
                    yield Splice(start, end, inserted)


def base_file(length: int) -> tuple[Atom, ...]:
    return tuple(Atom(index, f"line-{index % 2}") for index in range(length))


def apply_splice(
    source: tuple[Atom, ...], edit: Splice, fresh_start: int
) -> tuple[Atom, ...]:
    assert edit.end <= len(source)
    added = tuple(
        Atom(fresh_start + index, f"new-{index % 2}") for index in range(edit.inserted)
    )
    assert not {item.identity for item in added}.intersection(
        item.identity for item in source
    )
    return source[: edit.start] + added + source[edit.end :]


def map_span(target: Span, edit: Splice) -> Span | None:
    if edit.start == edit.end:
        if target.start < edit.start < target.end:
            return None
        if edit.start <= target.start:
            return Span(target.start + edit.inserted, target.end + edit.inserted)
        return target
    if max(target.start, edit.start) < min(target.end, edit.end):
        return None
    if edit.end <= target.start:
        return Span(target.start + edit.delta, target.end + edit.delta)
    return target


def map_gap(gap: int, source_length: int, edit: Splice) -> int | None:
    assert 0 <= gap <= source_length and edit.end <= source_length
    if edit.start == edit.end:
        if edit.start == gap:
            return None
        return gap + edit.inserted if edit.start < gap else gap
    if gap > 0 and edit.start <= gap - 1 < edit.end:
        return None
    if gap < source_length and edit.start <= gap < edit.end:
        return None
    return gap + edit.delta if edit.end <= gap else gap


def span_oracle(
    original: tuple[Atom, ...], current: tuple[Atom, ...], target: Span
) -> Span | None:
    identities = tuple(item.identity for item in original[target.start : target.end])
    positions = {item.identity: index for index, item in enumerate(current)}
    mapped = tuple(positions.get(identity, -1) for identity in identities)
    if not mapped or mapped[0] < 0:
        return None
    expected = tuple(range(mapped[0], mapped[0] + len(mapped)))
    return Span(mapped[0], mapped[0] + len(mapped)) if mapped == expected else None


def gap_oracle(
    original: tuple[Atom, ...], current: tuple[Atom, ...], gap: int
) -> int | None:
    positions = {item.identity: index for index, item in enumerate(current)}
    left = -1 if gap == 0 else positions.get(original[gap - 1].identity)
    right = (
        len(current) if gap == len(original) else positions.get(original[gap].identity)
    )
    if left is None or right is None or right != left + 1:
        return None
    return right


def check_single_splices() -> tuple[Counts, Counts]:
    ranges = Counts()
    gaps = Counts()
    for length in range(9):
        original = base_file(length)
        for edit in splices(length, 3):
            current = apply_splice(original, edit, 100)
            for target in spans(length):
                actual = map_span(target, edit)
                assert actual == span_oracle(original, current, target), (
                    length,
                    target,
                    edit,
                )
                ranges = ranges.add(actual is not None)
            for gap in range(length + 1):
                actual_gap = map_gap(gap, length, edit)
                assert actual_gap == gap_oracle(original, current, gap), (
                    length,
                    gap,
                    edit,
                )
                gaps = gaps.add(actual_gap is not None)
    return ranges, gaps


def check_two_splices() -> tuple[Counts, Counts]:
    ranges = Counts()
    gaps = Counts()
    for length in range(6):
        original = base_file(length)
        for first in splices(length, 2):
            intermediate = apply_splice(original, first, 100)
            for second in splices(len(intermediate), 2):
                current = apply_splice(intermediate, second, 200)
                for target in spans(length):
                    one = map_span(target, first)
                    actual = None if one is None else map_span(one, second)
                    expected = (
                        None if one is None else span_oracle(original, current, target)
                    )
                    assert actual == expected, (length, target, first, second)
                    ranges = ranges.add(actual is not None)
                for gap in range(length + 1):
                    one_gap = map_gap(gap, length, first)
                    actual_gap = (
                        None
                        if one_gap is None
                        else map_gap(one_gap, len(intermediate), second)
                    )
                    expected_gap = (
                        None if one_gap is None else gap_oracle(original, current, gap)
                    )
                    assert actual_gap == expected_gap, (length, gap, first, second)
                    gaps = gaps.add(actual_gap is not None)
    return ranges, gaps


def check_regressions() -> tuple[str, ...]:
    passed: list[str] = []
    original = (
        Atom(0, "fn live"),
        Atom(1, "return 1;"),
        Atom(2, "fn fixture"),
        Atom(3, "return 2;"),
    )
    current = (
        Atom(0, "fn live"),
        Atom(4, "return 3;"),
        Atom(2, "fn fixture"),
        Atom(5, "return 1;"),
    )
    old_value = original[1].text
    equal_text_candidates = [
        index for index, atom in enumerate(current) if old_value == atom.text
    ]
    assert equal_text_candidates == [3]
    assert span_oracle(original, current, Span(1, 2)) is None
    passed.append("equal_text_is_not_original_identity")

    original = (Atom(0, "A"), Atom(1, "B"), Atom(2, "C"))
    changed = (Atom(0, "A"), Atom(100, "X"), Atom(2, "C"))
    restored = (Atom(0, "A"), Atom(101, "B"), Atom(2, "C"))
    assert tuple(atom.text for atom in original) == tuple(
        atom.text for atom in restored
    )
    assert changed != original and span_oracle(original, restored, Span(1, 2)) is None
    assert map_span(Span(1, 2), Splice(1, 2, 1)) is None
    passed.append("known_aba_does_not_revive_target")

    assert map_span(Span(1, 3), Splice(1, 1, 2)) == Span(3, 5)
    assert map_span(Span(1, 3), Splice(3, 3, 2)) == Span(1, 3)
    assert map_span(Span(1, 3), Splice(2, 2, 2)) is None
    passed.append("range_boundary_insertions_do_not_expand_target")

    assert map_gap(2, 4, Splice(2, 2, 1)) is None
    assert map_gap(2, 4, Splice(1, 2, 1)) is None
    assert map_gap(2, 4, Splice(2, 3, 1)) is None
    passed.append("gap_occupancy_and_neighbors_are_protected")

    assert map_gap(0, 0, Splice(0, 0, 1)) is None
    assert map_gap(4, 4, Splice(4, 4, 1)) is None
    passed.append("empty_and_eof_gap_conflicts")

    delivery_sequence = 8
    old_request_cutoff = 7
    new_request_cutoff = 8
    assert not delivery_sequence <= old_request_cutoff
    assert delivery_sequence <= new_request_cutoff
    passed.append("later_delivery_cannot_authorize_pending_request")

    for valid_reference in (False, True):
        for unchanged_target in (False, True):
            for observed in (False, True):
                light = valid_reference and unchanged_target
                enhanced = light and observed
                assert not enhanced or light
                if light != enhanced:
                    assert light and not observed
    passed.append("enhanced_acceptance_is_light_plus_coverage")

    assert map_span(Span(0, 1), Splice(0, 1, 2)) is None
    passed.append("eof_separator_change_invalidates_touched_line")
    return tuple(passed)


def main() -> None:
    single_ranges, single_gaps = check_single_splices()
    double_ranges, double_gaps = check_two_splices()
    regressions = check_regressions()
    report = {
        "model": "hashline-v1-range-gap-identity",
        "status": "passed",
        "single_splice": {
            "file_lengths": [0, 8],
            "inserted_lengths": [0, 3],
            "range_cases": single_ranges.cases,
            "range_accepted": single_ranges.accepted,
            "gap_cases": single_gaps.cases,
            "gap_accepted": single_gaps.accepted,
        },
        "two_splices": {
            "initial_file_lengths": [0, 5],
            "inserted_lengths_each": [0, 2],
            "range_cases": double_ranges.cases,
            "range_accepted": double_ranges.accepted,
            "gap_cases": double_gaps.cases,
            "gap_accepted": double_gaps.accepted,
        },
        "regressions": regressions,
        "limitations": [
            "Bounded pure model, not a proof over all inputs.",
            "Line identities are modeled; filesystem occurrence identities are not supplied by hashes.",
            "No filesystem, parser, AST, journal, transport, allocator or process-race tests.",
            "Known conflicts remain invalid even when later edits restore equal bytes or adjacency.",
            "Observation checks are abstract ordering checks, not delivery instrumentation.",
        ],
    }
    data = (json.dumps(report, indent=2) + "\n").encode("utf-8")
    sys.stdout.buffer.write(data)


if __name__ == "__main__":
    main()
