import { describe, expect, test } from "vitest";
import corpus from "../../../../docs/dal/spec/input-text-cases.json";
import {
	isWhitespaceOnly,
	multilineText,
	searchEligibleChecked,
	singleLineFault,
	textFault,
} from "./fields";
import { scalarCount } from "./validate";

interface Vector {
	readonly id: string;
	readonly codePoints: readonly string[];
	readonly text: string;
	readonly scalarCount: number;
	readonly graphemeBoundaries: readonly number[];
	readonly policy: string;
}

interface Encoding {
	readonly id: string;
	readonly encoding: string;
	readonly input: string;
	readonly reason: string;
}

const vectors: readonly Vector[] = (
	corpus as unknown as { vectors: readonly Vector[] }
).vectors;
const invalidEncodings: readonly Encoding[] = (
	corpus as unknown as { invalidEncodings: readonly Encoding[] }
).invalidEncodings;

function decodedInvalid(input: string, encoding: string): string {
	if (encoding === "json_text") return JSON.parse(input) as string;
	return String.fromCharCode(Number.parseInt(input, 16));
}

describe("branch field policy", () => {
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
	test("single-line fields refuse only vectors with a line break", () => {
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
	test("browser strings reject invalid encodings", () => {
		const kept = invalidEncodings.filter((e) => e.encoding !== "utf8_hex");
		expect(kept).toHaveLength(4);
		for (const entry of kept) {
			const text = decodedInvalid(entry.input, entry.encoding);
			expect(textFault(text), entry.id).toBe(entry.reason);
			expect(singleLineFault(text), entry.id).toBe(entry.reason);
			expect(searchEligibleChecked(`abc${text}`), entry.id).toBe(false);
		}
	});
});

describe("branch line breaks", () => {
	const NEL = String.fromCodePoint(0x85);
	const LS = String.fromCodePoint(0x2028);
	const PS = String.fromCodePoint(0x2029);
	const NBSP = String.fromCodePoint(0xa0);
	test("multiline drafts convert CRLF and lone CR once", () => {
		expect(multilineText("a\r\nb\rc\n")).toBe("a\nb\nc\n");
		expect(multilineText("\r\r\n")).toBe("\n\n");
		expect(multilineText(`a${LS}b${PS}c${NEL}d\ve\ff`)).toBe(
			`a${LS}b${PS}c${NEL}d\ve\ff`,
		);
	});
	test("single-line fields refuse every declared line break", () => {
		for (const separator of ["\n", "\r", "\v", "\f", NEL, LS, PS]) {
			expect(singleLineFault(`one${separator}two`)).toBe(
				"line_break_not_allowed",
			);
		}
		expect(singleLineFault(`one\ttwo${NBSP}three`)).toBeNull();
	});
});

describe("branch eligibility", () => {
	const NBSP = String.fromCodePoint(0xa0);
	const LS = String.fromCodePoint(0x2028);
	const NEL = String.fromCodePoint(0x85);
	const ZWSP = String.fromCodePoint(0x200b);
	const COMBINING = String.fromCodePoint(0x301);
	const IDEOGRAPHIC = String.fromCodePoint(0x3000);
	const FULLWIDTH = String.fromCodePoint(0xff0f);
	const HANGUL = String.fromCodePoint(0xac00, 0xb098, 0xb2e4);
	const FLAG = String.fromCodePoint(0x1f1f0, 0x1f1f7);
	test("conversation search needs three scalars in every term", () => {
		const cases: ReadonlyArray<readonly [string, boolean]> = [
			["ab ", false],
			["abc de", false],
			[" abc ", true],
			["abc def", true],
			["", false],
			[` \t${IDEOGRAPHIC}`, false],
			[`${IDEOGRAPHIC}abc${IDEOGRAPHIC}`, true],
			[`ab${NBSP}cde`, false],
			[`ab${LS}cde`, false],
			[`abc${ZWSP}de`, true],
			[`e${COMBINING}e`, true],
			[FLAG, false],
			[HANGUL, true],
			[`${FULLWIDTH}ab`, true],
			[`abc${NEL}de`, false],
		];
		for (const [query, eligible] of cases) {
			expect(searchEligibleChecked(query), JSON.stringify(query)).toBe(
				eligible,
			);
		}
	});
	test("whitespace-only prompts use the White_Space property", () => {
		expect(isWhitespaceOnly("")).toBe(true);
		expect(isWhitespaceOnly(` \t\n${NBSP}${IDEOGRAPHIC}${NEL}${LS}`)).toBe(
			true,
		);
		expect(isWhitespaceOnly(ZWSP)).toBe(false);
		expect(isWhitespaceOnly(String.fromCodePoint(0xfeff))).toBe(false);
		expect(isWhitespaceOnly(" x ")).toBe(false);
	});
});
