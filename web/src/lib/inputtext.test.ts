import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import corpus from "../../../docs/dal/spec/input-text-cases.json";
import {
	deletionRange,
	displaySegments,
	isBlank,
	isWhitespaceOnly,
	multilineText,
	scalarCount,
	searchEligible,
	singleLineFault,
	textFault,
} from "./inputtext";

interface Vector {
	readonly id: string;
	readonly codePoints: readonly string[];
	readonly text: string;
	readonly scalarCount: number;
	readonly graphemeBoundaries: readonly number[];
	readonly policy: string;
}

interface Segmented {
	readonly name: string;
	readonly scalars: readonly number[];
	readonly boundaries: readonly number[];
}

const vectors: readonly Vector[] = corpus.vectors;

const graphemeBreakTest = fileURLToPath(
	new URL(
		"../../../internal/dal/inputtext/testdata/GraphemeBreakTest-18.0.0.txt",
		import.meta.url,
	),
);
const graphemeBreakTestSHA256 =
	"b0cf047ee94485bbdc846de2b902f5f8a815f6b674f9d04223cddadd91c9df31";

function segmentedVector(vector: Vector): Segmented {
	return {
		name: vector.id,
		scalars: vector.codePoints.map((hex) => Number.parseInt(hex, 16)),
		boundaries: vector.graphemeBoundaries,
	};
}

function segmentedLine(line: string): Segmented | null {
	const body = line.split("#")[0]?.trim() ?? "";
	if (body === "") return null;
	const scalars: number[] = [];
	const boundaries: number[] = [];
	for (const token of body.split(/\s+/)) {
		if (token === "÷") boundaries.push(scalars.length);
		else if (token !== "×") scalars.push(Number.parseInt(token, 16));
	}
	return { name: body, scalars, boundaries };
}

function unitOffsets(scalars: readonly number[]): readonly number[] {
	const offsets = [0];
	for (const scalar of scalars) {
		offsets.push((offsets.at(-1) ?? 0) + (scalar > 0xffff ? 2 : 1));
	}
	return offsets;
}

function deletionMismatches(segmented: Segmented): readonly string[] {
	const text = String.fromCodePoint(...segmented.scalars);
	const units = unitOffsets(segmented.scalars);
	const mismatches: string[] = [];
	for (let index = 1; index < segmented.boundaries.length; index += 1) {
		const first = segmented.boundaries[index - 1] ?? 0;
		const last = segmented.boundaries[index] ?? 0;
		const expected = JSON.stringify({ start: units[first], end: units[last] });
		for (let scalar = first; scalar <= last; scalar += 1) {
			const caret = units[scalar] ?? 0;
			const backward = JSON.stringify(deletionRange(text, caret, "backward"));
			const forward = JSON.stringify(deletionRange(text, caret, "forward"));
			if (scalar > first && backward !== expected) {
				mismatches.push(`${segmented.name} backward from ${scalar}`);
			}
			if (scalar < last && forward !== expected) {
				mismatches.push(`${segmented.name} forward from ${scalar}`);
			}
		}
	}
	return mismatches;
}

function decodedInvalid(input: string, encoding: string): string {
	if (encoding === "json_text") return JSON.parse(input) as string;
	return String.fromCharCode(Number.parseInt(input, 16));
}

describe("shared corpus", () => {
	test("each vector spells its declared code points", () => {
		for (const vector of vectors) {
			const scalars = vector.codePoints.map((hex) => Number.parseInt(hex, 16));
			expect(vector.text, vector.id).toBe(String.fromCodePoint(...scalars));
		}
	});

	test("the generic policy keeps every accepted vector unchanged", () => {
		for (const vector of vectors.filter((entry) => entry.policy === "accept")) {
			expect(textFault(vector.text), vector.id).toBeNull();
			expect(multilineText(vector.text), vector.id).toBe(vector.text);
		}
	});

	test("the multiline adapter converts raw CRLF to one LF once", () => {
		const raw = vectors.find(
			(entry) => entry.policy === "multiline_adapter_lf",
		);
		if (raw === undefined) throw new Error("the corpus lost its CRLF vector");
		expect(textFault(raw.text)).toBeNull();
		expect(multilineText(raw.text)).toBe("\n");
		expect(multilineText(multilineText(raw.text))).toBe("\n");
	});

	test("NUL fails as a disallowed control", () => {
		const rejected = vectors.filter(
			(entry) => entry.policy === "reject_disallowed_control",
		);
		expect(rejected.map((entry) => entry.id)).toEqual(["nul"]);
		for (const vector of rejected) {
			expect(textFault(vector.text), vector.id).toBe("disallowed_control");
			expect(singleLineFault(vector.text), vector.id).toBe(
				"disallowed_control",
			);
		}
	});

	test("single-line fields refuse only the vectors that hold a line break", () => {
		const broken = new Set(["raw_crlf", "leading_trailing_whitespace"]);
		for (const vector of vectors.filter(
			(entry) => entry.policy !== "reject_disallowed_control",
		)) {
			expect(singleLineFault(vector.text), vector.id).toBe(
				broken.has(vector.id) ? "line_break_not_allowed" : null,
			);
		}
	});

	test("scalar counts match the corpus", () => {
		for (const vector of vectors) {
			expect(scalarCount(vector.text), vector.id).toBe(vector.scalarCount);
		}
	});

	test("Backspace and Delete remove the whole cluster at and inside corpus boundaries", () => {
		expect(
			vectors.flatMap((vector) => deletionMismatches(segmentedVector(vector))),
		).toEqual([]);
	});

	test("deletion past either end removes nothing", () => {
		for (const vector of vectors) {
			expect(deletionRange(vector.text, 0, "backward"), vector.id).toBeNull();
			expect(
				deletionRange(vector.text, vector.text.length, "forward"),
				vector.id,
			).toBeNull();
		}
	});

	test("browser strings reject the corpus's UTF-16 and JSON invalid encodings", () => {
		const browserEncodings = corpus.invalidEncodings.filter(
			(entry) => entry.encoding !== "utf8_hex",
		);
		expect(browserEncodings).toHaveLength(4);
		for (const entry of browserEncodings) {
			const text = decodedInvalid(entry.input, entry.encoding);
			expect(textFault(text), entry.id).toBe(entry.reason);
			expect(textFault(`a${text}b`), entry.id).toBe(entry.reason);
			expect(singleLineFault(text), entry.id).toBe(entry.reason);
			expect(searchEligible(`abc${text}`), entry.id).toBe(false);
		}
	});
});

describe("Unicode 18.0.0 GraphemeBreakTest", () => {
	const source = readFileSync(graphemeBreakTest);
	const lines = source
		.toString("utf8")
		.split("\n")
		.map(segmentedLine)
		.filter((line) => line !== null);

	test("the vendored test file matches its pin", () => {
		expect(createHash("sha256").update(source).digest("hex")).toBe(
			graphemeBreakTestSHA256,
		);
		expect(lines).toHaveLength(853);
	});

	test("Backspace and Delete remove the whole cluster on every line", () => {
		expect(lines.flatMap(deletionMismatches)).toEqual([]);
	});
});

describe("line breaks", () => {
	test("multiline drafts convert CRLF and lone CR once and keep the final LF", () => {
		expect(multilineText("a\r\nb\rc\n")).toBe("a\nb\nc\n");
		expect(multilineText("\r\r\n")).toBe("\n\n");
		expect(multilineText("a\u2028b\u2029c\u0085d\ve\ff")).toBe(
			"a\u2028b\u2029c\u0085d\ve\ff",
		);
	});

	test("single-line fields refuse every declared line break", () => {
		for (const separator of [
			"\n",
			"\r",
			"\v",
			"\f",
			"\u0085",
			"\u2028",
			"\u2029",
		]) {
			expect(singleLineFault(`one${separator}two`)).toBe(
				"line_break_not_allowed",
			);
		}
		expect(singleLineFault("one\ttwo\u00a0three")).toBeNull();
	});
});

describe("eligibility", () => {
	test("conversation search needs three scalars in every whitespace-separated term", () => {
		const cases: readonly (readonly [string, boolean])[] = [
			["ab ", false],
			["abc de", false],
			[" abc ", true],
			["abc def", true],
			["", false],
			[" \t\u3000", false],
			["\u3000abc\u3000", true],
			["ab\u00a0cde", false],
			["ab\u2028cde", false],
			["abc\u200bde", true],
			["e\u0301e", true],
			["\u{1F1F0}\u{1F1F7}", false],
			["\uac00\ub098\ub2e4", true],
			["\uff0fab", true],
			["abc\u0085de", false],
		];
		for (const [query, eligible] of cases) {
			expect(searchEligible(query), JSON.stringify(query)).toBe(eligible);
		}
	});

	test("whitespace-only prompts use the Unicode White_Space property", () => {
		expect(isWhitespaceOnly("")).toBe(true);
		expect(isWhitespaceOnly(" \t\n\u00a0\u3000\u0085\u2028")).toBe(true);
		expect(isWhitespaceOnly("\u200b")).toBe(false);
		expect(isWhitespaceOnly("\ufeff")).toBe(false);
		expect(isWhitespaceOnly(" x ")).toBe(false);
	});

	test("required fields are blank only with invisible scalars and complete escapes", () => {
		const cases: readonly (readonly [string, boolean])[] = [
			["", true],
			[" \t\n\r\u00a0\u1680\u2000\u2028\u2029\u202f\u205f\u3000\u0085", true],
			["\x00\x07\x7f\u009b", true],
			[
				"\u00ad\u034f\u061c\u115f\u180e\u200b\u200d\u2060\u2064\u3164\ufe0f\ufeff\uffa0\u{1BCA0}\u{E0001}\u{E0100}\u{E0FFF}",
				true,
			],
			["\x1b[2J\x1b]0;title\x07\x1bPq#0\x1b\\\x1b_x\x1b\\\x1b7 ", true],
			["\x1b[?25 q\x1b]104\x07\x1bP1$r\x1b\\\x1b^\x1b\\\x1b(B\x1b\\", true],
			["\u202e\u200d\ufe0f", true],
			["x", false],
			[" x ", false],
			["\u0301", false],
			["\ue000", false],
			["\u{F0001}", false],
			["\ufffd", false],
			["\u2800", false],
			["\ufdd0", false],
			["\uff0f", false],
			["\x1b]0;title", false],
			["\x1b[", false],
			["\x1b[1;2", false],
			["\x1b[1\x18m", false],
			["\x1b]0;a\x1b[31m\x07", false],
			["\x1b]0;a\x1a\x07", false],
			["\x1b]l title\x1b\\", false],
			["\x1b];title\x07", false],
			["\x1b]0title\x07", false],
			["\x1bP\x1b\\", false],
			["\x1bPqa\x07", false],
			["\x1bXabc\x07", false],
			["\x1b(", false],
			["\x1b\u00e9", false],
		];
		for (const [text, blank] of cases) {
			expect(isBlank(text), JSON.stringify(text)).toBe(blank);
		}
	});
});

describe("display escapes", () => {
	test("controls and bidirectional formatting get visible code-point escapes", () => {
		expect(
			displaySegments(
				"a\x1b[2J\u202eb\nc\td\r\u0085e\u{1F469}\u200d\u{1F4BB}",
				"controls",
			),
		).toEqual([
			{ text: "a", escape: null },
			{ text: "\x1b", escape: "U+001B" },
			{ text: "[2J", escape: null },
			{ text: "\u202e", escape: "U+202E" },
			{ text: "b\nc\td", escape: null },
			{ text: "\r", escape: "U+000D" },
			{ text: "\u0085", escape: "U+0085" },
			{ text: "e\u{1F469}\u200d\u{1F4BB}", escape: null },
		]);
	});

	test("revealing invisible scalars also escapes default-ignorable ones", () => {
		expect(displaySegments("Yes\u200b\u{E0001}", "invisible")).toEqual([
			{ text: "Yes", escape: null },
			{ text: "\u200b", escape: "U+200B" },
			{ text: "\u{E0001}", escape: "U+E0001" },
		]);
		expect(displaySegments("Yes\u200b", "controls")).toEqual([
			{ text: "Yes\u200b", escape: null },
		]);
	});

	test("joined segments spell the original text", () => {
		for (const vector of vectors) {
			for (const reveal of ["controls", "invisible"] as const) {
				const segments = displaySegments(vector.text, reveal);
				expect(
					segments.map((segment) => segment.text).join(""),
					vector.id,
				).toBe(vector.text);
			}
		}
	});
});
