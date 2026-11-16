import { describe, expect, test } from "vitest";
import {
	approvalGlyphs,
	contextFraction,
	effortGlyph,
	folderGlyph,
	queueGlyph,
} from "#lib/band.ts";
import type { TokenBreakdown, TokenUsage } from "#lib/types.ts";

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

describe("context fraction", () => {
	const breakdown: TokenBreakdown = {
		totalTokens: 7032,
		inputTokens: 7032,
		cachedInputTokens: 0,
		cacheWriteInputTokens: 0,
		outputTokens: 0,
		reasoningOutputTokens: 0,
	};

	function usage(overrides: Partial<TokenUsage> = {}): TokenUsage {
		return {
			last: breakdown,
			total: breakdown,
			contextWindow: 32000,
			estimatedContextTokens: null,
			...overrides,
		};
	}

	test("prefers the journal estimate over the last provider usage", () => {
		expect(contextFraction(usage({ estimatedContextTokens: 2179 }))).toBe(
			2179 / 32000,
		);
	});

	test("uses an estimate of zero instead of the last provider usage", () => {
		const fraction = contextFraction(usage({ estimatedContextTokens: 0 }));
		expect(fraction).toBe(0);
		expect(fraction).not.toBe(7032 / 32000);
	});

	test("falls back to the last provider usage without an estimate", () => {
		expect(contextFraction(usage())).toBe(7032 / 32000);
	});

	test("clamps an over-window estimate to one", () => {
		expect(contextFraction(usage({ estimatedContextTokens: 40000 }))).toBe(1);
	});

	test("returns null without a positive context window", () => {
		expect(contextFraction(usage({ contextWindow: null }))).toBeNull();
		expect(contextFraction(usage({ contextWindow: 0 }))).toBeNull();
		expect(contextFraction(usage({ contextWindow: -32000 }))).toBeNull();
	});
});
