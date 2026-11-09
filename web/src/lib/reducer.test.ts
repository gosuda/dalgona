import { describe, expect, test } from "vitest";
import { advisorLabel, itemStatusLabel, kindLabel } from "./labels";
import {
	activeApproval,
	emptyTranscript,
	itemDisplayStatus,
	itemsAwaitingApproval,
	otherThreadApprovalCount,
	reduceNotification,
	resolveApprovalRequest,
	trackApprovalRequest,
	transcriptForThread,
} from "./reducer";
import type {
	AppNotification,
	ApprovalRequest,
	ThreadDetail,
	TranscriptItem,
} from "./types";

function agent(id: string, text: string, complete: boolean): TranscriptItem {
	return { id, kind: "agentMessage", text, complete };
}

function command(id: string, output: string): TranscriptItem {
	return {
		id,
		kind: "commandExecution",
		command: "ls",
		output,
		status: "inProgress",
		exitCode: null,
	};
}

function thread(turns: ThreadDetail["turns"]): ThreadDetail {
	return {
		summary: {
			id: "thread-1",
			title: "notes",
			preview: "notes",
			status: "idle",
			updatedAt: 10,
			model: "gpt",
			cwd: "/work",
		},
		turns,
	};
}

describe("transcript reducer", () => {
	test("concatenates deltas onto the matching item without mutating the previous list", () => {
		const start = emptyTranscript("thread-1");
		const seeded = reduceNotification(start, {
			type: "itemStarted",
			threadId: "thread-1",
			turnId: "turn-1",
			item: agent("item-1", "hel", false),
		});
		const before = seeded.items;
		const next = reduceNotification(seeded, {
			type: "delta",
			channel: "agent",
			threadId: "thread-1",
			turnId: "turn-1",
			itemId: "item-1",
			delta: "lo",
		});
		expect(before).toEqual([agent("item-1", "hel", false)]);
		expect(next.items).toEqual([agent("item-1", "hello", false)]);
		expect(next.items).not.toBe(before);
	});

	test("replaces the whole item on completion instead of appending a second copy", () => {
		const seeded = reduceNotification(emptyTranscript("thread-1"), {
			type: "itemStarted",
			threadId: "thread-1",
			turnId: "turn-1",
			item: agent("item-1", "partial", false),
		});
		const done = agent("item-1", "final answer", true);
		const next = reduceNotification(seeded, {
			type: "itemCompleted",
			threadId: "thread-1",
			turnId: "turn-1",
			item: done,
		});
		expect(next.items).toEqual([done]);
	});

	test("tracks turn status from started through completed and keeps the turn error", () => {
		const started = reduceNotification(emptyTranscript("thread-1"), {
			type: "turnStarted",
			threadId: "thread-1",
			turn: { id: "turn-1", status: "inProgress", items: [], error: null },
		});
		expect(started.turnId).toBe("turn-1");
		expect(started.turnStatus).toBe("inProgress");
		const completed = reduceNotification(started, {
			type: "turnCompleted",
			threadId: "thread-1",
			turn: { id: "turn-1", status: "failed", items: [], error: "boom" },
		});
		expect(completed.turnStatus).toBe("failed");
		expect(completed.error).toBe("boom");
		expect(started.turnStatus).toBe("inProgress");
	});

	test("hydrates turns in order and keeps the last turn status", () => {
		const state = transcriptForThread(
			thread([
				{
					id: "turn-1",
					status: "completed",
					items: [agent("a", "one", true), command("c", "out")],
					error: null,
				},
				{
					id: "turn-2",
					status: "interrupted",
					items: [agent("b", "two", true)],
					error: "stopped",
				},
			]),
		);
		expect(state.items.map((item) => item.id)).toEqual(["a", "c", "b"]);
		expect(state.turnId).toBe("turn-2");
		expect(state.turnStatus).toBe("interrupted");
		expect(state.error).toBe("stopped");
	});

	test("drops notifications for a different thread", () => {
		const state = emptyTranscript("thread-1");
		const foreign: AppNotification = {
			type: "delta",
			channel: "agent",
			threadId: "thread-2",
			turnId: "turn-9",
			itemId: "item-9",
			delta: "nope",
		};
		expect(reduceNotification(state, foreign)).toBe(state);
	});

	test("creates an in-progress item when a delta arrives before item/started", () => {
		const next = reduceNotification(emptyTranscript("thread-1"), {
			type: "delta",
			channel: "command",
			threadId: "thread-1",
			turnId: "turn-1",
			itemId: "cmd-1",
			delta: "hi",
		});
		expect(next.items).toEqual([
			{
				id: "cmd-1",
				kind: "commandExecution",
				command: "",
				output: "hi",
				status: "inProgress",
				exitCode: null,
			},
		]);
		const grown = reduceNotification(next, {
			type: "delta",
			channel: "command",
			threadId: "thread-1",
			turnId: "turn-1",
			itemId: "cmd-1",
			delta: "!",
		});
		expect(grown.items[0]).toMatchObject({ output: "hi!" });
		const started = reduceNotification(grown, {
			type: "itemStarted",
			threadId: "thread-1",
			turnId: "turn-1",
			item: {
				id: "cmd-1",
				kind: "commandExecution",
				command: "pwd",
				output: "",
				status: "inProgress",
				exitCode: null,
			},
		});
		expect(started.items).toEqual([
			{
				id: "cmd-1",
				kind: "commandExecution",
				command: "pwd",
				output: "hi!",
				status: "inProgress",
				exitCode: null,
			},
		]);
	});

	test("keeps the latest plan for the current turn and clears it on a new turn", () => {
		const steps = [{ step: "fix", status: "inProgress" as const }];
		const planned = reduceNotification(emptyTranscript("thread-1"), {
			type: "plan",
			threadId: "thread-1",
			turnId: "turn-1",
			explanation: null,
			steps,
		});
		expect(planned.plan).toEqual({
			turnId: "turn-1",
			explanation: null,
			steps,
		});
		const otherThread = reduceNotification(planned, {
			type: "plan",
			threadId: "thread-2",
			turnId: "turn-9",
			explanation: null,
			steps: [],
		});
		expect(otherThread).toBe(planned);
		const turn = { status: "inProgress" as const, items: [], error: null };
		const sameTurn = reduceNotification(planned, {
			type: "turnStarted",
			threadId: "thread-1",
			turn: { ...turn, id: "turn-1" },
		});
		expect(sameTurn.plan).toEqual(planned.plan);
		const nextTurn = reduceNotification(planned, {
			type: "turnStarted",
			threadId: "thread-1",
			turn: { ...turn, id: "turn-2" },
		});
		expect(nextTurn.plan).toBeNull();
	});
});

describe("approval tracking", () => {
	const request: ApprovalRequest = {
		id: 7,
		method: "item/commandExecution/requestApproval",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "cmd-1",
		title: "Run ls",
		detail: "ls",
		reason: null,
	};

	function statusLabel(
		approvals: readonly ApprovalRequest[],
		itemId: string,
	): string {
		return itemStatusLabel(
			itemDisplayStatus(
				"inProgress",
				itemId,
				itemsAwaitingApproval(approvals, "thread-1"),
			),
		);
	}

	test("labels the requested item as waiting until the request resolves", () => {
		const pending = trackApprovalRequest([], request);
		expect(statusLabel(pending, "cmd-1")).toBe("Waiting for approval");
		expect(statusLabel(pending, "cmd-2")).toBe("Running");
		const resolved = resolveApprovalRequest(pending, request.id);
		expect(statusLabel(resolved, "cmd-1")).toBe("Running");
	});

	test("keeps finished items and other threads on their own status", () => {
		const pending = trackApprovalRequest([], {
			...request,
			method: "item/fileChange/requestApproval",
		});
		const awaiting = itemsAwaitingApproval(pending, "thread-1");
		expect(itemDisplayStatus("completed", "cmd-1", awaiting)).toBe("completed");
		expect(itemsAwaitingApproval(pending, "thread-2").has("cmd-1")).toBe(false);
	});
});

describe("active approval filtering", () => {
	const first: ApprovalRequest = {
		id: 7,
		method: "item/commandExecution/requestApproval",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "cmd-1",
		title: "Run ls",
		detail: "ls",
		reason: null,
	};
	const other: ApprovalRequest = { ...first, id: 8, threadId: "thread-2" };

	test("shows the approval that belongs to the active thread", () => {
		expect(activeApproval([other, first], "thread-1")).toBe(first);
		expect(activeApproval([other, first], "thread-2")).toBe(other);
	});

	test("shows nothing when no approval belongs to the active thread", () => {
		expect(activeApproval([other], "thread-1")).toBeNull();
		expect(activeApproval([other], null)).toBeNull();
	});

	test("counts approvals waiting on other threads", () => {
		expect(otherThreadApprovalCount([first, other], "thread-1")).toBe(1);
		expect(otherThreadApprovalCount([first, other], "thread-3")).toBe(2);
	});
});

describe("labels", () => {
	test("summarizes advisor state as text", () => {
		expect(advisorLabel({ kind: "unknown" })).toBeNull();
		expect(advisorLabel({ kind: "off" })).toBe("off");
		const advisor = { name: "advisor", model: "gpt", notes: 1 };
		expect(
			advisorLabel({
				kind: "loaded",
				advisors: [{ ...advisor, status: "reviewing" }],
			}),
		).toBe("reviewing · 1 note");
		expect(
			advisorLabel({
				kind: "loaded",
				advisors: [
					{ ...advisor, status: "reviewing" },
					{ ...advisor, name: "b", status: "disabled", notes: 0 },
				],
			}),
		).toBe("2 advisors · 1 reviewing");
	});

	test("names statuses and item kinds in plain words", () => {
		expect(itemStatusLabel("inProgress")).toBe("Running");
		expect(itemStatusLabel("custom")).toBe("custom");
		expect(kindLabel("webSearch")).toBe("Web search");
		expect(kindLabel("imageGeneration")).toBe("Image generation");
	});
});
