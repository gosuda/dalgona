import { describe, expect, test } from "vitest";
import {
	approvalGlyphs,
	effortGlyph,
	folderGlyph,
	queueGlyph,
} from "#lib/band.ts";

function expectGlyph(text: string | undefined, codePoint: number): void {
	const glyphs = [...(text ?? "")];
	expect(glyphs).toHaveLength(1);
	expect(glyphs[0]?.codePointAt(0)).toBe(codePoint);
}

describe("band glyphs", () => {
	test("folder glyph is the md-folder_outline code point", () => {
		expectGlyph(folderGlyph, 0xf0256);
	});

	test("queue glyph is the md-timer_sand code point", () => {
		expectGlyph(queueGlyph, 0xf051f);
	});

	test("effort glyphs run from empty to full gauge over four levels", () => {
		expectGlyph(effortGlyph(0, 4), 0xf0873);
		expectGlyph(effortGlyph(1, 4), 0xf0875);
		expectGlyph(effortGlyph(2, 4), 0xf029a);
		expectGlyph(effortGlyph(3, 4), 0xf0874);
	});

	test("approval glyphs are the ask, auto and yolo code points", () => {
		expectGlyph(approvalGlyphs["ask"], 0xf0e47);
		expectGlyph(approvalGlyphs["auto"], 0xf167a);
		expectGlyph(approvalGlyphs["yolo"], 0xf0238);
	});
});
