import {
	cleanup,
	fireEvent,
	render,
	waitFor,
	within,
} from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import Transcript from "#lib/components/Transcript.svelte";
import type { OutputPage, TranscriptItem } from "#lib/types.ts";

function props(items: readonly TranscriptItem[]) {
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
		childThreadIds: [],
		threads: [],
		userAgent: null,
		starting: false,
		onOpenThread: vi.fn(),
		onStart: vi.fn(),
		readOutput: vi.fn(),
	};
}

const evalCall: TranscriptItem = {
	id: "call-1",
	kind: "toolCall",
	name: "eval",
	arguments: {
		code: "tools.todo(items = [])\ntools.web_fetch(url = 'https://example.com')",
	},
	output: "",
	status: "completed",
	durationMs: null,
};

const ls: TranscriptItem = {
	id: "run-1/2",
	kind: "commandExecution",
	command: "ls",
	output: "a.txt",
	status: "completed",
	exitCode: 0,
	durationMs: null,
};

function dialog(): HTMLDialogElement {
	const found = document.querySelector("dialog");
	if (found === null) throw new Error("no dialog in the document");
	return found;
}

function spilled(id: string): TranscriptItem {
	return {
		id,
		kind: "commandExecution",
		command: `cat ${id}`,
		output: "",
		status: "completed",
		exitCode: 0,
		durationMs: null,
		spill: { path: `blobs/${id}`, bytes: 8, sha256: "0".repeat(64) },
	};
}

async function openRow(index: number): Promise<void> {
	const row = document.querySelectorAll<HTMLElement>(".items > li")[index];
	if (row === undefined) throw new Error(`no transcript row ${index}`);
	await fireEvent.click(
		within(row).getByRole("button", { name: "Open full text" }),
	);
}

afterEach(cleanup);

describe("review view", () => {
	test("Ctrl+O opens the full output of the latest command", async () => {
		const output = Array.from(
			{ length: 30 },
			(_, index) => `line ${index + 1}`,
		);
		render(Transcript, {
			props: props([
				{
					id: "c1",
					kind: "commandExecution",
					command: "seq 30",
					output: output.join("\n"),
					status: "completed",
					exitCode: 0,
					durationMs: null,
				},
			]),
		});
		document.getElementById("transcript")?.focus();
		await fireEvent.keyDown(document.activeElement ?? document.body, {
			key: "o",
			ctrlKey: true,
		});
		expect(dialog().open).toBe(true);
		expect(dialog().textContent).toContain("line 30");
		expect(dialog().textContent).toContain("line 1\n");
	});

	test("opens the full code and output of an eval row", async () => {
		render(Transcript, { props: props([evalCall, ls]) });
		const rows = Array.from(document.querySelectorAll(".items > li"));
		const evalRow = rows[0];
		const commandRow = rows[1];
		if (evalRow === undefined || commandRow === undefined) {
			throw new Error("expected an eval row and a command row");
		}

		await fireEvent.click(
			within(evalRow as HTMLElement).getByRole("button", {
				name: "Open full text",
			}),
		);
		expect(dialog().open).toBe(true);
		expect(dialog().textContent).toContain("tools.todo(items = [])");
		expect(dialog().textContent).toContain(
			"tools.web_fetch(url = 'https://example.com')",
		);
		await fireEvent.keyDown(dialog(), { key: "Escape" });
		expect(dialog().open).toBe(false);

		await fireEvent.click(
			within(commandRow as HTMLElement).getByRole("button", {
				name: "Open full text",
			}),
		);
		expect(dialog().textContent).toContain("$ ls");
		expect(dialog().textContent).toContain("a.txt");
	});

	test("Escape closes the review and returns focus to the control that opened it", async () => {
		render(Transcript, { props: props([ls]) });
		const button = within(
			document.querySelector(".items > li") as HTMLElement,
		).getByRole("button", { name: "Open full text" });
		button.focus();
		await fireEvent.click(button);
		expect(dialog().open).toBe(true);
		await fireEvent.keyDown(dialog(), { key: "Escape" });
		expect(dialog().open).toBe(false);
		expect(document.activeElement).toBe(button);
	});

	test("a command at medium shows its last ten lines and says how many it hides", () => {
		const output = Array.from(
			{ length: 30 },
			(_, index) => `line ${index + 1}`,
		);
		const { container } = render(Transcript, {
			props: {
				...props([
					{
						id: "c2",
						kind: "commandExecution",
						command: "seq 30",
						output: output.join("\n"),
						status: "completed",
						exitCode: 0,
						durationMs: null,
					},
				]),
				verbosity: "medium",
			},
		});
		const text = container.querySelector("pre.output")?.textContent ?? "";
		expect(text).not.toContain("line 1\n");
		expect(text).toContain("line 21");
		expect(text).toContain("line 30");
		expect(container.textContent).toContain("Showing the last 10 of 30 lines");
	});

	test("a failed output load shows its error for that item only", async () => {
		const pending = Promise.withResolvers<OutputPage>();
		const readOutput = vi.fn((itemId: string) => {
			if (itemId === "a") return Promise.reject(new Error("disk gone"));
			return pending.promise;
		});
		render(Transcript, {
			props: { ...props([spilled("a"), spilled("b")]), readOutput },
		});
		await openRow(0);
		await waitFor(() =>
			expect(dialog().textContent).toContain(
				"Could not load the full output: disk gone",
			),
		);
		expect(dialog().textContent).not.toContain("Loading the full output");
		await fireEvent.keyDown(dialog(), { key: "Escape" });
		await openRow(1);
		await waitFor(() =>
			expect(dialog().textContent).toContain("Loading the full output"),
		);
		expect(dialog().textContent).not.toContain("disk gone");
		expect(dialog().textContent).not.toContain("Could not load");
		await waitFor(() => expect(readOutput).toHaveBeenCalledWith("b", 0));
		pending.resolve({ text: "b output", offset: 0, next: 8, total: 8 });
		await waitFor(() => expect(dialog().textContent).toContain("b output"));
		expect(dialog().textContent).not.toContain("Could not load");
	});

	test("Ctrl+O inside the open review keeps the review on its item", async () => {
		render(Transcript, {
			props: props([
				ls,
				{ ...ls, id: "run-1/3", command: "pwd", output: "/repo" },
			]),
		});
		await openRow(0);
		expect(dialog().textContent).toContain("$ ls");
		dialog().focus();
		await fireEvent.keyDown(dialog(), { key: "o", ctrlKey: true });
		expect(dialog().textContent).toContain("$ ls");
		expect(dialog().textContent).not.toContain("$ pwd");
	});

	test("the full text joins every page the output reader returns", async () => {
		const readOutput = vi.fn((_itemId: string, offset: number) =>
			Promise.resolve(
				offset === 0
					? { text: "page one\n", offset: 0, next: 9, total: 17 }
					: { text: "page two", offset: 9, next: 17, total: 17 },
			),
		);
		render(Transcript, { props: { ...props([spilled("a")]), readOutput } });
		await openRow(0);
		await waitFor(() =>
			expect(dialog().textContent).toContain("page one\npage two"),
		);
		expect(readOutput).toHaveBeenCalledTimes(2);
		expect(readOutput).toHaveBeenNthCalledWith(2, "a", 9);
	});

	test("Ctrl+O with focus on the page body opens nothing", async () => {
		render(Transcript, { props: props([ls]) });
		(document.activeElement as HTMLElement | null)?.blur();
		expect(document.activeElement).toBe(document.body);
		await fireEvent.keyDown(document.body, { key: "o", ctrlKey: true });
		expect(document.querySelector("dialog")?.open ?? false).toBe(false);
	});
});
