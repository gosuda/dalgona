import { describe, expect, test } from "vitest";
import {
	activeApproval,
	emptyTranscript,
	itemsAwaitingApproval,
	type KnownChildThreads,
	otherThreadApprovalCount,
	recordChildThread,
	reduceNotification,
	trackApprovalRequest,
	transcriptForThread,
} from "./reducer";
import type { ApprovalRequest, ThreadDetail } from "./types";
import { parseNotification } from "./validation";

const childRequest: ApprovalRequest = {
	id: "child-request-1",
	method: "item/commandExecution/requestApproval",
	threadId: "child-9",
	turnId: "turn-9",
	itemId: "cmd-9",
	title: "Run ls",
	detail: "ls",
	reason: null,
};

function spawned(threadId: string, childId: string) {
	return {
		type: "childSpawned" as const,
		threadId,
		childId,
	};
}

function parentDetail(id: string): ThreadDetail {
	return {
		summary: {
			id,
			title: "notes",
			preview: "notes",
			status: "idle",
			updatedAt: 10,
			model: "gpt",
			cwd: "/work",
		},
		turns: [],
	};
}

describe("child approvals", () => {
	test("parses the child spawned notification", () => {
		expect(
			parseNotification({
				method: "dalgona/thread/childSpawned",
				params: { threadId: "thread-1", childId: "child-9", cwd: "/tmp" },
			}),
		).toEqual({
			type: "childSpawned",
			threadId: "thread-1",
			childId: "child-9",
		});
	});

	test("records child threads of the open thread only", () => {
		const opened = reduceNotification(
			emptyTranscript("thread-1"),
			spawned("thread-1", "child-9"),
		);
		expect(opened.childThreadIds).toEqual(["child-9"]);
		const foreign = reduceNotification(
			emptyTranscript("thread-2"),
			spawned("thread-1", "child-9"),
		);
		expect(foreign.childThreadIds).toEqual([]);
	});

	test("shows a child approval in the open parent thread", () => {
		const approvals = trackApprovalRequest([], childRequest);
		const transcript = reduceNotification(
			emptyTranscript("thread-1"),
			spawned("thread-1", "child-9"),
		);
		expect(
			activeApproval(approvals, "thread-1", transcript.childThreadIds),
		).toStrictEqual(childRequest);
		expect(
			otherThreadApprovalCount(
				approvals,
				"thread-1",
				transcript.childThreadIds,
			),
		).toBe(0);
		expect(
			itemsAwaitingApproval(
				approvals,
				"thread-1",
				transcript.childThreadIds,
			).has("cmd-9"),
		).toBe(true);
	});

	test("still hides approvals from unrelated threads", () => {
		const approvals = trackApprovalRequest([], {
			...childRequest,
			threadId: "thread-2",
		});
		expect(activeApproval(approvals, "thread-1", ["child-9"])).toBeNull();
		expect(otherThreadApprovalCount(approvals, "thread-1", ["child-9"])).toBe(
			1,
		);
	});

	test("keeps a child approval visible after the parent reopens", () => {
		let known: KnownChildThreads = {};
		known = recordChildThread(known, "thread-1", "child-9");
		const approvals = trackApprovalRequest([], childRequest);
		const reopened = transcriptForThread(parentDetail("thread-1"), known);
		expect(
			activeApproval(approvals, reopened.threadId, reopened.childThreadIds),
		).toStrictEqual(childRequest);
		expect(
			otherThreadApprovalCount(
				approvals,
				reopened.threadId,
				reopened.childThreadIds,
			),
		).toBe(0);
		expect(
			itemsAwaitingApproval(
				approvals,
				reopened.threadId,
				reopened.childThreadIds,
			).has("cmd-9"),
		).toBe(true);
	});

	test("scopes reopened child links to the reopened thread", () => {
		let known: KnownChildThreads = {};
		known = recordChildThread(known, "thread-1", "child-9");
		const reopened = transcriptForThread(parentDetail("thread-2"), known);
		expect(reopened.childThreadIds).toEqual([]);
	});
});
