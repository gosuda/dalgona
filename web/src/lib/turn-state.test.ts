import { describe, expect, test } from "vitest";
import { emptyTranscript, reduceNotification } from "./reducer";

describe("late turn start", () => {
	test("a start response keeps a completed turn terminal", () => {
		const completed = reduceNotification(emptyTranscript("thread-1"), {
			type: "turnCompleted",
			threadId: "thread-1",
			turn: { id: "turn-1", status: "completed", items: [], error: null },
		});
		const late = reduceNotification(completed, {
			type: "turnStarted",
			threadId: "thread-1",
			turn: { id: "turn-1", status: "inProgress", items: [], error: null },
		});
		expect(late.turnId).toBe("turn-1");
		expect(late.turnStatus).toBe("completed");
		expect(late.error).toBeNull();
	});

	test("a start response keeps a failed turn and its error", () => {
		const failed = reduceNotification(emptyTranscript("thread-1"), {
			type: "turnCompleted",
			threadId: "thread-1",
			turn: { id: "turn-1", status: "failed", items: [], error: "boom" },
		});
		const late = reduceNotification(failed, {
			type: "turnStarted",
			threadId: "thread-1",
			turn: { id: "turn-1", status: "inProgress", items: [], error: null },
		});
		expect(late.turnStatus).toBe("failed");
		expect(late.error).toBe("boom");
	});

	test("a start for a new turn still applies after a terminal turn", () => {
		const completed = reduceNotification(emptyTranscript("thread-1"), {
			type: "turnCompleted",
			threadId: "thread-1",
			turn: { id: "turn-1", status: "completed", items: [], error: null },
		});
		const next = reduceNotification(completed, {
			type: "turnStarted",
			threadId: "thread-1",
			turn: { id: "turn-2", status: "inProgress", items: [], error: null },
		});
		expect(next.turnId).toBe("turn-2");
		expect(next.turnStatus).toBe("inProgress");
	});
});
