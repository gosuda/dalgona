import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, test } from "vitest";
import DiffCard from "#lib/components/DiffCard.svelte";
import type { FileChange } from "#lib/types.ts";

afterEach(cleanup);

describe("diff card", () => {
	test("shows counts as text and marks lines with ins and del", () => {
		const change: FileChange = {
			path: "a.txt",
			kind: "update",
			diff: "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
			additions: 1,
			deletions: 1,
			diffOmitted: false,
		};
		const { container } = render(DiffCard, { props: { change, open: true } });
		expect(container.querySelector("summary")?.textContent).toContain("+1 −1");
		expect(container.querySelector("ins")?.textContent).toBe("+new");
		expect(container.querySelector("del")?.textContent).toBe("-old");
		expect(container.querySelector(".hunk")?.textContent).toBe("@@ -1 +1 @@");
	});

	test("says when the server omitted the diff", () => {
		const change: FileChange = {
			path: "big.bin",
			kind: "update",
			diff: "",
			diffOmitted: true,
		};
		const { container } = render(DiffCard, {
			props: { change, open: false },
		});
		expect(container.textContent).toContain("Diff not shown.");
		expect(container.querySelector("pre")).toBeNull();
	});

	test("shows the added count alone when the server sends no deletions", () => {
		const change: FileChange = {
			path: "a.txt",
			kind: "add",
			diff: "@@ -0,0 +1 @@\n+new\n",
			additions: 1,
			diffOmitted: false,
		};
		const { container } = render(DiffCard, { props: { change, open: true } });
		expect(container.querySelector(".counts")?.textContent).toBe("+1");
	});

	test("shows 60 of 61 diff lines and says that one line is hidden", () => {
		const body = Array.from({ length: 60 }, (_, index) => `+line ${index + 1}`);
		const change: FileChange = {
			path: "long.txt",
			kind: "add",
			diff: `@@ -0,0 +1,60 @@\n${body.join("\n")}\n`,
			additions: 60,
			deletions: 0,
			diffOmitted: false,
		};
		const { container } = render(DiffCard, { props: { change, open: true } });
		const text = container.querySelector("pre.diff")?.textContent ?? "";
		const rows = text.split("\n");
		expect(rows).toHaveLength(61);
		expect(rows[0]).toBe("@@ -0,0 +1,60 @@");
		expect(rows[60]).toBe("… 1 more line");
	});
});
