import { describe, expect, test } from "vitest";
import {
	displayBlock,
	displayLine,
	distinctLines,
	exposed,
} from "#lib/text/display.ts";

describe("text display", () => {
	test("a line turns breaks and tabs into spaces and escapes controls", () => {
		const cases: [string, string][] = [
			["  a\tb\nc  ", "a b c"],
			["a\r\nb", "a b"],
			["a b\u0085c", "a b c"],
			["A‮B‬C", "A<U+202E>B<U+202C>C"],
			["\u0000\u007f\u009b", "<U+0000><U+007F><U+009B>"],
			["\u001b[1mBold", "<U+001B>[1mBold"],
			["\u{F0001}﷐�", "\u{F0001}﷐�"],
			["é 가", "é 가"],
			["　x　", "x"],
		];
		for (const [text, want] of cases) expect(displayLine(text)).toBe(want);
	});

	test("a block keeps lines, expands tabs, and drops leading blank lines", () => {
		expect(displayBlock("\n \n\tcode\r\nnext end  ")).toBe(
			"    code\nnext\nend",
		);
	});

	test("an exposed label escapes every scalar but printable ASCII", () => {
		expect(exposed("Yes​")).toBe("Yes<U+200B>");
		expect(exposed("<U+00E9>")).toBe("<U+003C>U+00E9>");
		expect(exposed("a bé")).toBe("a<U+0020>b<U+00E9>");
		expect(exposed("\u{1F468}‍")).toBe("<U+1F468><U+200D>");
	});

	test("labels that differ only in hidden scalars show their escapes", () => {
		expect(
			distinctLines(["Yes", "Yes​", "No", " No ", "Maybe", "\u001b[1mMaybe"]),
		).toEqual([
			"Yes",
			"Yes<U+200B>",
			"No",
			"<U+0020>No<U+0020>",
			"Maybe",
			"<U+001B>[1mMaybe",
		]);
		expect(distinctLines(["Same", "Same"])).toEqual(["Same", "Same"]);
	});
});
