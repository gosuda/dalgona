import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import Transcript from "#lib/components/Transcript.svelte";
import type { ThreadSummary, TranscriptItem } from "#lib/types.ts";

function props(
	items: readonly TranscriptItem[],
	extra: {
		readonly childThreadIds?: readonly string[];
		readonly threads?: readonly ThreadSummary[];
		readonly onOpenThread?: (threadId: string) => void;
	} = {},
) {
	return {
		threadId: "thread-1",
		items,
		turnStatus: "completed" as const,
		error: null,
		plan: null,
		turnReview: null,
		connected: true,
		loading: false,
		awaitingApproval: new Set<string>(),
		childThreadIds: extra.childThreadIds ?? [],
		threads: extra.threads ?? [],
		userAgent: null,
		starting: false,
		onOpenThread: extra.onOpenThread ?? vi.fn(),
		onStart: vi.fn(),
	};
}

afterEach(cleanup);

describe("transcript rows", () => {
	test("renders each item kind with its state word", () => {
		const { container } = render(Transcript, {
			props: props([
				{
					id: "u1",
					kind: "userMessage",
					text: "list files",
					turnId: "turn-1",
					keywords: [],
				},
				{ id: "r1", kind: "reasoning", text: "looking", complete: true },
				{
					id: "c1",
					kind: "commandExecution",
					command: "ls",
					output: "a.txt",
					status: "completed",
					exitCode: 0,
					durationMs: 1500,
				},
				{
					id: "f1",
					kind: "fileChange",
					changes: [
						{
							path: "a.txt",
							diff: "-old\n+new",
							kind: "update",
							diffOmitted: false,
						},
					],
					status: "completed",
					review: null,
				},
				{
					id: "t1",
					kind: "toolCall",
					name: "web_fetch",
					arguments: { url: "https://example.com" },
					output: "",
					status: "failed",
					durationMs: null,
				},
				{ id: "p1", kind: "plan", text: "read the code" },
				{ id: "k1", kind: "contextCompaction" },
			]),
		});
		const text = container.textContent ?? "";
		expect(text).toContain("Thinking");
		expect(text).toContain("$ ls");
		expect(text).toContain("exit 0");
		expect(text).toContain("1s");
		expect(text).toContain("Done");
		expect(text).toContain("File changes");
		expect(text).toContain("web_fetch");
		expect(text).toContain("Failed");
		expect(text).toContain("read the code");
		expect(text).toContain("Context compacted");
		expect(container.querySelector(".failed")?.textContent).toBe("Failed");
	});

	test("keyword matches carry an underline mark and are listed as words", () => {
		const { container } = render(Transcript, {
			props: props([
				{
					id: "u1",
					kind: "userMessage",
					text: "please ultrathink now",
					turnId: "turn-1",
					keywords: [{ word: "ultrathink", byteRange: { start: 7, end: 17 } }],
				},
			]),
		});
		expect(container.querySelector("mark.keyword")?.textContent).toBe(
			"ultrathink",
		);
		expect(container.querySelector(".keywords")?.textContent).toBe(
			"Keywords: ultrathink",
		);
	});

	test("subagent rows open the child thread", async () => {
		const onOpenThread = vi.fn();
		render(Transcript, {
			props: props([], {
				childThreadIds: ["child-1"],
				threads: [
					{
						id: "child-1",
						title: "Count files",
						preview: "",
						status: "idle",
						updatedAt: 0,
						model: null,
						cwd: "/work",
					},
				],
				onOpenThread,
			}),
		});
		expect(screen.getByText("Count files")).toBeTruthy();
		await fireEvent.click(screen.getByRole("button", { name: "Open" }));
		expect(onOpenThread).toHaveBeenCalledWith("child-1");
	});

	test("renders an eval call and each nested call as its own row, in arrival order", () => {
		const { container } = render(Transcript, {
			props: props([
				{
					id: "call-1",
					kind: "toolCall",
					name: "eval",
					arguments: {
						code: "tools.todo(items = [])\ntools.web_fetch(url = 'https://example.com')",
					},
					output: "",
					status: "completed",
					durationMs: null,
				},
				{
					id: "run-1/1",
					kind: "toolCall",
					name: "todo",
					arguments: { items: [] },
					output: "",
					status: "completed",
					durationMs: null,
				},
				{
					id: "run-1/2",
					kind: "commandExecution",
					command: "ls",
					output: "a.txt",
					status: "completed",
					exitCode: 0,
					durationMs: null,
				},
				{
					id: "run-1/3",
					kind: "commandExecution",
					command: "pwd",
					output: "/repo",
					status: "completed",
					exitCode: 0,
					durationMs: null,
				},
				{
					id: "run-1/4",
					kind: "toolCall",
					name: "web_fetch",
					arguments: { url: "https://example.com" },
					output: "ok",
					status: "completed",
					durationMs: null,
				},
			]),
		});
		const rows = Array.from(container.querySelectorAll(".items > li"));
		expect(rows).toHaveLength(5);
		expect(rows[0]?.querySelector(".item-label")?.textContent).toBe("eval");
		expect(rows[1]?.querySelector(".item-label")?.textContent).toBe("todo");
		expect(rows[2]?.querySelector(".command")?.textContent).toBe("$ ls");
		expect(rows[3]?.querySelector(".command")?.textContent).toBe("$ pwd");
		expect(rows[4]?.querySelector(".item-label")?.textContent).toBe(
			"web_fetch",
		);
		expect(rows[2]?.textContent).not.toContain("pwd");
		expect(rows[3]?.textContent).not.toContain("ls");
	});

	test("renders identical file changes as two rows", () => {
		const change = {
			path: "a.txt",
			diff: "-old\n+new",
			kind: "update" as const,
			diffOmitted: false,
		};
		const { container } = render(Transcript, {
			props: props([
				{
					id: "f1",
					kind: "fileChange",
					changes: [change, change],
					status: "completed",
					review: null,
				},
			]),
		});
		expect(container.querySelectorAll(".file-change")).toHaveLength(2);
	});

	test("renders a rewrite change as a file-change row", () => {
		const { container } = render(Transcript, {
			props: props([
				{
					id: "f2",
					kind: "fileChange",
					changes: [
						{
							path: "b.txt",
							kind: "update",
							diff: "--- a/b.txt\n+++ b/b.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n",
							additions: 1,
							deletions: 1,
							diffOmitted: false,
						},
					],
					status: "completed",
					review: null,
				},
			]),
		});
		const rows = container.querySelectorAll(".file-change");
		expect(rows).toHaveLength(1);
		const summary = rows[0]?.querySelector("summary")?.textContent ?? "";
		expect(summary).toContain("Changed");
		expect(summary).toContain("b.txt");
		expect(summary).toContain("+1 −1");
		expect(rows[0]?.querySelector("del")?.textContent).toBe("-beta");
		expect(rows[0]?.querySelector("ins")?.textContent).toBe("+gamma");
	});

	test("shows the edit review under a file change and the turn review after the rows", () => {
		const { container } = render(Transcript, {
			props: {
				...props([
					{
						id: "f3",
						kind: "fileChange",
						changes: [],
						status: "completed",
						review: { verdict: "flagged", summary: "Flagged: removed code" },
					},
				]),
				turnReview: { verdict: "uncertain", summary: "Uncertain: incomplete" },
			},
		});
		const editReview = container.querySelector(".edit-review");
		expect(editReview?.getAttribute("data-verdict")).toBe("flagged");
		expect(editReview?.textContent).toBe("Flagged: removed code");
		const turnReview = container.querySelector(".turn-review");
		expect(turnReview?.getAttribute("data-verdict")).toBe("uncertain");
		expect(turnReview?.textContent).toBe("Uncertain: incomplete");
	});
});
