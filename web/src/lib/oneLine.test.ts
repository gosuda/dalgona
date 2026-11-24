import { describe, expect, test } from "vitest";
import { hasLineBreak } from "#lib/oneLine.ts";

describe("hasLineBreak", () => {
	test("finds each single-line break", () => {
		for (const brk of ["\n", "\r", "\v", "\f", "\u0085", " ", " "]) {
			expect(hasLineBreak(`a${brk}b`)).toBe(true);
		}
	});

	test("keeps tabs, spaces, and other White_Space on one line", () => {
		expect(hasLineBreak(" a\tb 　c ")).toBe(false);
	});
});
