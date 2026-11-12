import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, test } from "vitest";
import FileChangeRow from "#lib/components/rows/FileChangeRow.svelte";
import type { FileChange, TranscriptItem } from "#lib/types.ts";
import { presentRow } from "#lib/verbosity.ts";

afterEach(cleanup);

describe("file change row", () => {
	test("at medium shows 60 diff lines and says how many it hides", () => {
		const body = Array.from({ length: 80 }, (_, index) => `+line ${index + 1}`);
		const change: FileChange = {
			path: "long.txt",
			kind: "add",
			diff: `@@ -0,0 +1,80 @@\n${body.join("\n")}\n`,
			additions: 80,
			deletions: 0,
			diffOmitted: false,
		};
		const item: TranscriptItem = {
			id: "f1",
			kind: "fileChange",
			changes: [change],
			status: "completed",
			review: null,
		};
		const { container } = render(FileChangeRow, {
			props: {
				item,
				running: false,
				awaiting: new Set<string>(),
				presentation: presentRow(item, "medium", false),
			},
		});
		const text = container.querySelector("pre.diff")?.textContent ?? "";
		const rows = text.split("\n");
		expect(rows).toHaveLength(61);
		expect(rows[0]).toBe("@@ -0,0 +1,80 @@");
		expect(rows[60]).toBe("… 21 more lines");
		expect(text).toContain("+line 59");
		expect(text).not.toContain("+line 60");
	});
});
