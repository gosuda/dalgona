import { describe, expect, it } from "vitest";
import { parseDiff } from "./diff.js";

describe("parseDiff", () => {
	it("classifies header lines only before the first hunk", () => {
		expect(
			parseDiff("--- a/x.txt\n+++ b/x.txt\n@@ -1 +1 @@\n--- old\n+++ new\n"),
		).toEqual([
			{ kind: "meta", text: "--- a/x.txt" },
			{ kind: "meta", text: "+++ b/x.txt" },
			{ kind: "hunk", text: "@@ -1 +1 @@" },
			{ kind: "del", text: "--- old" },
			{ kind: "add", text: "+++ new" },
		]);
	});

	it("classifies a unified diff", () => {
		const applyPatch =
			"--- a/a.go\n+++ b/a.go\n@@ -1,3 +1,3 @@\n package a\n-func Old() {}\n+func New() {}\n func Keep() {}\n";
		const rewrite =
			"--- a/b.txt\n+++ b/b.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n";
		expect(parseDiff(applyPatch).map((line) => line.kind)).toEqual([
			"meta",
			"meta",
			"hunk",
			"context",
			"del",
			"add",
			"context",
		]);
		expect(parseDiff(rewrite).map((line) => line.kind)).toEqual([
			"meta",
			"meta",
			"hunk",
			"context",
			"del",
			"add",
		]);
	});

	it("gives no lines for an empty diff", () => {
		expect(parseDiff("")).toEqual([]);
	});
});
