import { describe, expect, it } from "vitest";
import { findKeywords, keywordGradient } from "./keywords.js";
import type { KeywordDefinition } from "./types.js";

const ultra: KeywordDefinition = { word: "ultrathink", hue: [20, 50] };
const glow: KeywordDefinition = { word: "glow", hue: [200, 230] };

function words(
	prompt: string,
	table: readonly KeywordDefinition[] = [ultra],
): readonly string[] {
	return findKeywords(prompt, table).map((hit) => hit.word);
}

describe("findKeywords", () => {
	it("matches a standalone word beside punctuation", () => {
		expect(words("please ultrathink.")).toEqual(["ultrathink"]);
		expect(words("(ultrathink), now")).toEqual(["ultrathink"]);
		expect(words("ultrathink!")).toEqual(["ultrathink"]);
		expect(findKeywords("ultrathink", [ultra])[0]).toEqual({
			word: "ultrathink",
			start: 0,
			end: 10,
			hue: [20, 50],
		});
	});

	it("returns every repeated match in text order", () => {
		const hits = findKeywords("ultrathink then ultrathink", [ultra]);
		expect(hits.map((hit) => [hit.start, hit.end])).toEqual([
			[0, 10],
			[16, 26],
		]);
	});

	it("rejects glued, dotted, namespaced, call, and uppercase forms", () => {
		expect(words("ultrathinking")).toEqual([]);
		expect(words("x.ultrathink")).toEqual([]);
		expect(words("namespace::ultrathink")).toEqual([]);
		expect(words("ultrathink(")).toEqual([]);
		expect(words("ULTRATHINK")).toEqual([]);
	});

	it("ignores inline code, fences, tags, and comments", () => {
		expect(words("use `ultrathink` later")).toEqual([]);
		expect(words("```ts\nultrathink\n```")).toEqual([]);
		expect(words("~~~\nultrathink\n~~~")).toEqual([]);
		expect(words("<!-- ultrathink -->")).toEqual([]);
		expect(words('<span class="ultrathink">keep</span>')).toEqual([]);
		expect(words("<ultrathink>keep</ultrathink>")).toEqual([]);
		expect(words("before `ultrathink` after ultrathink")).toEqual([
			"ultrathink",
		]);
		expect(words("<!-- ultrathink --> ultrathink")).toEqual(["ultrathink"]);
		expect(words("<b>ultrathink</b>")).toEqual(["ultrathink"]);
	});

	it("rejects dot, slash, and hyphen paths", () => {
		expect(words("ultrathink.ts")).toEqual([]);
		expect(words("./ultrathink")).toEqual([]);
		expect(words("src/ultrathink")).toEqual([]);
		expect(words("src\\ultrathink")).toEqual([]);
		expect(words("ultrathink/now")).toEqual([]);
		expect(words("ultrathink-now")).toEqual([]);
		expect(words("foo.ultrathink.bar")).toEqual([]);
		expect(words("ultrathink.")).toEqual(["ultrathink"]);
	});

	it("treats unicode letters and digits as boundaries", () => {
		expect(words("한ultrathink")).toEqual([]);
		expect(words("ultrathink한")).toEqual([]);
		expect(words("٣ultrathink")).toEqual([]);
		expect(words("ultrathink３")).toEqual([]);
		expect(words("한 ultrathink")).toEqual(["ultrathink"]);
	});

	it("fires only for words in the enabled table", () => {
		expect(words("ultrathink glow", [glow])).toEqual(["glow"]);
		expect(words("ultrathink", [])).toEqual([]);
		expect(
			findKeywords("glow ultrathink", [ultra, glow]).map((hit) => hit.word),
		).toEqual(["glow", "ultrathink"]);
	});

	it("counts preceding emoji as one code point", () => {
		const prompt = "😀ultrathink";
		const hit = findKeywords(prompt, [ultra])[0];
		expect(prompt.indexOf("ultrathink")).toBe(2);
		expect(hit).toEqual({
			word: "ultrathink",
			start: 1,
			end: 11,
			hue: [20, 50],
		});
		if (hit === undefined) {
			return;
		}
		expect(Array.from(prompt).slice(hit.start, hit.end).join("")).toBe(
			"ultrathink",
		);
	});
});

describe("keywordGradient", () => {
	it("builds fourteen constant-lightness oklch stops across the hue pair", () => {
		const gradient = keywordGradient([10, 140]);
		expect(gradient.match(/oklch\(/g)?.length).toBe(14);
		expect(
			gradient.startsWith(
				"linear-gradient(90deg, oklch(0.82 0.13 10.00) 0.00%",
			),
		).toBe(true);
		expect(gradient.endsWith("oklch(0.82 0.13 140.00) 100.00%)")).toBe(true);
		expect(gradient.split("oklch(0.82 0.13").length - 1).toBe(14);
	});
});
