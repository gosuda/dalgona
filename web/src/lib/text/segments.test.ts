import { describe, expect, test } from "vitest";
import corpus from "../../../../docs/dal/spec/input-text-cases.json";
import { displaySegments, isVisuallyBlank } from "./segments";

interface Vector {
	readonly id: string;
	readonly text: string;
}

const vectors: readonly Vector[] = (
	corpus as unknown as { vectors: readonly Vector[] }
).vectors;

describe("branch display escapes", () => {
	test("controls and bidi get visible code-point escapes", () => {
		const ESC = String.fromCodePoint(0x1b);
		const BIDI = String.fromCodePoint(0x202e);
		const CR = String.fromCodePoint(0xd);
		const NEL = String.fromCodePoint(0x85);
		const WOMAN = String.fromCodePoint(0x1f469);
		const ZWJ = String.fromCodePoint(0x200d);
		const LAPTOP = String.fromCodePoint(0x1f4bb);
		expect(
			displaySegments(
				`a${ESC}[2J${BIDI}b\nc\td${CR}${NEL}e${WOMAN}${ZWJ}${LAPTOP}`,
				"controls",
			),
		).toEqual([
			{ text: "a", escape: null },
			{ text: ESC, escape: "U+001B" },
			{ text: "[2J", escape: null },
			{ text: BIDI, escape: "U+202E" },
			{ text: "b\nc\td", escape: null },
			{ text: CR, escape: "U+000D" },
			{ text: NEL, escape: "U+0085" },
			{ text: `e${WOMAN}${ZWJ}${LAPTOP}`, escape: null },
		]);
	});
	test("revealing invisible also escapes default-ignorable ones", () => {
		const ZWSP = String.fromCodePoint(0x200b);
		const TAG = String.fromCodePoint(0xe0001);
		expect(displaySegments(`Yes${ZWSP}${TAG}`, "invisible")).toEqual([
			{ text: "Yes", escape: null },
			{ text: ZWSP, escape: "U+200B" },
			{ text: TAG, escape: "U+E0001" },
		]);
		expect(displaySegments(`Yes${ZWSP}`, "controls")).toEqual([
			{ text: `Yes${ZWSP}`, escape: null },
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

describe("branch visually blank", () => {
	const NBSP = String.fromCodePoint(0xa0);
	const OGHAM = String.fromCodePoint(0x1680);
	const ENQUAD = String.fromCodePoint(0x2000);
	const LS = String.fromCodePoint(0x2028);
	const PS = String.fromCodePoint(0x2029);
	const NNBSP = String.fromCodePoint(0x202f);
	const MMSP = String.fromCodePoint(0x205f);
	const IDEOGRAPHIC = String.fromCodePoint(0x3000);
	const NEL = String.fromCodePoint(0x85);
	const SHY = String.fromCodePoint(0xad);
	const CGJ = String.fromCodePoint(0x34f);
	const ISOLATE = String.fromCodePoint(0x61c);
	const FILLER = String.fromCodePoint(0x115f);
	const MONGOLIAN = String.fromCodePoint(0x180e);
	const ZWSP = String.fromCodePoint(0x200b);
	const ZWJ = String.fromCodePoint(0x200d);
	const WJ = String.fromCodePoint(0x2060);
	const INVISIBLE_PLUS = String.fromCodePoint(0x2064);
	const HANGUL_FILLER = String.fromCodePoint(0x3164);
	const VS16 = String.fromCodePoint(0xfe0f);
	const BOM = String.fromCodePoint(0xfeff);
	const HALFWIDTH_FILLER = String.fromCodePoint(0xffa0);
	const SHORTHAND = String.fromCodePoint(0x1bca0);
	const TAG_START = String.fromCodePoint(0xe0001);
	const TAG_A = String.fromCodePoint(0xe0100);
	const TAG_END = String.fromCodePoint(0xe0fff);
	const BIDI = String.fromCodePoint(0x202e);
	const COMBINING = String.fromCodePoint(0x301);
	const PRIVATE = String.fromCodePoint(0xe000);
	const EXTRA_PRIVATE = String.fromCodePoint(0xf0001);
	const NONCHARACTER = String.fromCodePoint(0xfdd0);
	const FULLWIDTH_SOLIDUS = String.fromCodePoint(0xff0f);
	const REPLACEMENT = String.fromCodePoint(0xfffd);
	const BRAILLE_BLANK = String.fromCodePoint(0x2800);
	const ESC = String.fromCodePoint(0x1b);
	const E_ACUTE = String.fromCodePoint(0xe9);
	const CAN = String.fromCodePoint(0x18);
	const SUB = String.fromCodePoint(0x1a);
	const CSI = String.fromCodePoint(0x9b);
	const NUL = String.fromCodePoint(0x0);
	const BEL = String.fromCodePoint(0x7);
	const DEL = String.fromCodePoint(0x7f);
	const BACKSLASH = String.fromCodePoint(0x5c);
	test("blank means only invisible scalars and complete escapes", () => {
		const whitespace = ` \t\n\r${NBSP}${OGHAM}${ENQUAD}${LS}${PS}${NNBSP}${MMSP}${IDEOGRAPHIC}${NEL}`;
		const controls = `${NUL}${BEL}${DEL}${CSI}`;
		const invisibles = `${SHY}${CGJ}${ISOLATE}${FILLER}${MONGOLIAN}${ZWSP}${ZWJ}${WJ}${INVISIBLE_PLUS}${HANGUL_FILLER}${VS16}${BOM}${HALFWIDTH_FILLER}${SHORTHAND}${TAG_START}${TAG_A}${TAG_END}`;
		const titleEscape = `${ESC}[2J${ESC}]0;title${BEL}${ESC}Pq#0${ESC}${BACKSLASH}${ESC}_x${ESC}${BACKSLASH}${ESC}7 `;
		const modeEscape = `${ESC}[?25 q${ESC}]104${BEL}${ESC}P1$r${ESC}${BACKSLASH}${ESC}^${ESC}${BACKSLASH}${ESC}(B${ESC}${BACKSLASH}`;
		const bidiOnly = `${BIDI}${ZWJ}${VS16}`;
		const blankCases: readonly string[] = [
			"",
			whitespace,
			controls,
			invisibles,
			titleEscape,
			modeEscape,
			bidiOnly,
		];
		for (const text of blankCases) {
			expect(isVisuallyBlank(text), JSON.stringify(text)).toBe(true);
		}
		const contentCases: readonly string[] = [
			"x",
			" x ",
			COMBINING,
			PRIVATE,
			EXTRA_PRIVATE,
			REPLACEMENT,
			BRAILLE_BLANK,
			NONCHARACTER,
			FULLWIDTH_SOLIDUS,
			`${ESC}]0;title`,
			`${ESC}[`,
			`${ESC}[1;2`,
			`${ESC}[1${CAN}m`,
			`${ESC}]0;a${ESC}[31m${BEL}`,
			`${ESC}]0;a${SUB}${BEL}`,
			`${ESC}]l title${ESC}${BACKSLASH}`,
			`${ESC}];title${BEL}`,
			`${ESC}]0title${BEL}`,
			`${ESC}P${ESC}${BACKSLASH}`,
			`${ESC}Pqa${BEL}`,
			`${ESC}Xabc${BEL}`,
			`${ESC}(`,
			`${ESC}${E_ACUTE}`,
		];
		for (const text of contentCases) {
			expect(isVisuallyBlank(text), JSON.stringify(text)).toBe(false);
		}
	});
});
