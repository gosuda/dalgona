import { describe, expect, test } from "vitest";
import { threadHue } from "#lib/hue.ts";

function fnv1a32Oracle(text: string): number {
	let hash = 2166136261n;
	for (const byte of new TextEncoder().encode(text)) {
		hash = ((hash ^ BigInt(byte)) * 16777619n) & 0xffffffffn;
	}
	return Number(hash % 4n);
}

describe("threadHue", () => {
	test("the empty id hashes to the FNV offset basis", () => {
		expect(threadHue("")).toBe(1);
	});

	test("the known FNV-1a value of a single letter gives hue 0", () => {
		expect(threadHue("a")).toBe(0);
	});

	test("matches an independent BigInt FNV-1a for each id", () => {
		for (const id of [
			"thread-1",
			"thread-2",
			"é",
			"スレッド",
			"t-long-identifier-0042",
		]) {
			expect(threadHue(id)).toBe(fnv1a32Oracle(id));
		}
	});
});
