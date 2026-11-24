import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import {
	graphemeBoundaries,
	nextBoundary,
	previousBoundary,
} from "./graphemes";
import {
	answerScalars,
	checkText,
	isBlank,
	newlinesToLF,
	searchEligible,
} from "./validate";

const repo = (path: string): string =>
	fileURLToPath(new URL(`../../../../${path}`, import.meta.url));

interface BreakCase {
	line: number;
	text: string;
	boundaries: number[];
}

function breakCases(): BreakCase[] {
	const data = readFileSync(
		repo("internal/dal/textinput/testdata/GraphemeBreakTest-18.0.0.txt"),
	);
	expect(createHash("sha256").update(data).digest("hex")).toBe(
		"b0cf047ee94485bbdc846de2b902f5f8a815f6b674f9d04223cddadd91c9df31",
	);
	const cases: BreakCase[] = [];
	data
		.toString("utf8")
		.split("\n")
		.forEach((raw, index) => {
			const fields = (raw.split("#")[0] ?? "")
				.trim()
				.split(/\s+/)
				.filter(Boolean);
			if (fields.length === 0) return;
			let text = "";
			const boundaries: number[] = [];
			for (const field of fields) {
				if (field === "\u00f7") boundaries.push(text.length);
				else if (field !== "\u00d7")
					text += String.fromCodePoint(Number.parseInt(field, 16));
			}
			cases.push({ line: index + 1, text, boundaries });
		});
	expect(cases.length).toBeGreaterThan(700);
	return cases;
}

describe("Unicode 18.0.0 grapheme conformance", () => {
	const cases = breakCases();

	test("boundaries match every GraphemeBreakTest line", () => {
		const failures = cases.filter(
			(c) => graphemeBoundaries(c.text).join() !== c.boundaries.join(),
		);
		expect(failures.map((c) => c.line)).toEqual([]);
	});

	test("caret steps match both ways", () => {
		for (const c of cases) {
			const forward = [0];
			for (let i = 0; i < c.text.length; ) {
				i = nextBoundary(c.text, i);
				forward.push(i);
			}
			const backward = [c.text.length];
			for (let i = c.text.length; i > 0; ) {
				i = previousBoundary(c.text, i);
				backward.unshift(i);
			}
			expect([c.line, forward]).toEqual([
				c.line,
				c.text.length ? c.boundaries : [0],
			]);
			expect([c.line, backward]).toEqual([
				c.line,
				c.text.length ? c.boundaries : [0],
			]);
		}
	});
});

interface Vector {
	id: string;
	text: string;
	scalarCount: number;
	utf8Bytes: number;
	utf16Units: number;
	graphemeBoundaries: number[];
	policy: string;
}

interface Encoding {
	id: string;
	encoding: string;
	input: string;
	reason: string;
}

const corpus = JSON.parse(
	readFileSync(repo("docs/dal/spec/input-text-cases.json"), "utf8"),
) as {
	unicodeVersion: string;
	vectors: Vector[];
	invalidEncodings: Encoding[];
};

describe("shared input corpus", () => {
	test("targets the pinned Unicode version", () => {
		expect(corpus.unicodeVersion).toBe("18.0.0");
	});

	test.each(corpus.vectors)("$id keeps its counts and boundaries", (v) => {
		expect([...v.text].length).toBe(v.scalarCount);
		expect(new TextEncoder().encode(v.text).length).toBe(v.utf8Bytes);
		expect(v.text.length).toBe(v.utf16Units);
		const scalarBoundaries = graphemeBoundaries(v.text).map(
			(b) => [...v.text.slice(0, b)].length,
		);
		expect(scalarBoundaries).toEqual(v.graphemeBoundaries);
		const reason = checkText(v.text);
		if (v.policy === "reject_disallowed_control")
			expect(reason).toBe("disallowed_control");
		else expect(reason).toBeUndefined();
		if (v.policy === "multiline_adapter_lf") {
			const converted = newlinesToLF(v.text);
			expect(converted.includes("\r")).toBe(false);
			expect(newlinesToLF(converted)).toBe(converted);
		}
	});

	test.each(corpus.invalidEncodings.filter((e) => e.encoding === "utf16_hex"))(
		"$id fails before TextEncoder can replace it",
		(e) => {
			const text = String.fromCharCode(Number.parseInt(e.input, 16));
			expect(checkText(text)).toBe(e.reason);
		},
	);
});

describe("field rules", () => {
	test("the answer quota counts scalars and rejects whole", () => {
		const answer = { maxScalars: answerScalars };
		expect(checkText("a".repeat(2000), answer)).toBeUndefined();
		expect(checkText("a".repeat(2001), answer)).toBe("text_too_large");
		expect(checkText("\u{1F600}".repeat(2000), answer)).toBeUndefined();
		expect(checkText(`a${"\u0301".repeat(2000)}`, answer)).toBe(
			"text_too_large",
		);
		expect(checkText(`a${"\u0301".repeat(100000)}`, answer)).toBe(
			"text_too_large",
		);
	});

	test("single-line fields refuse every line break", () => {
		for (const br of ["\n", "\r", "\v", "\f", "\u0085", "\u2028", "\u2029"]) {
			expect(checkText(`a${br}b`, { singleLine: true })).toBe(
				"line_break_not_allowed",
			);
		}
	});

	test("newline conversion changes CRLF and CR once", () => {
		expect(newlinesToLF("a\r\nb\rc\n\u2028")).toBe("a\nb\nc\n\u2028");
	});

	test("blank and search eligibility use White_Space", () => {
		expect(isBlank(" \u3000\t")).toBe(true);
		expect(isBlank(" \u200b ")).toBe(false);
		expect(searchEligible("abc")).toBe(true);
		expect(searchEligible("abc de")).toBe(false);
		expect(searchEligible("  ")).toBe(false);
		expect(searchEligible("\u{20000}\u{20000} abc")).toBe(false);
		expect(searchEligible("\ud55c\uad6d\uc5b4\u3000\uac80\uc0c9\uc5b4")).toBe(
			true,
		);
	});
});
