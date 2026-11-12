import { describe, expect, it } from "vitest";
import { RpcError } from "./protocol.js";
import { parseAdvisorStatusResponse, parseNotification } from "./validation.js";

describe("notification validation", () => {
	it("parses a completed turn", () => {
		expect(
			parseNotification({
				method: "turn/completed",
				params: {
					threadId: "thread-1",
					turn: { id: "turn-1", status: "completed", items: [], error: null },
				},
			}),
		).toEqual({
			type: "turnCompleted",
			threadId: "thread-1",
			turn: {
				id: "turn-1",
				status: "completed",
				items: [],
				error: null,
				startedAtMs: null,
			},
		});
	});

	it("parses a completed agent item as a full replacement", () => {
		expect(
			parseNotification({
				method: "item/completed",
				params: {
					completedAtMs: 2,
					threadId: "thread-1",
					turnId: "turn-1",
					item: { type: "agentMessage", id: "item-1", text: "finished" },
				},
			}),
		).toEqual({
			type: "itemCompleted",
			threadId: "thread-1",
			turnId: "turn-1",
			item: {
				kind: "agentMessage",
				id: "item-1",
				text: "finished",
				complete: true,
			},
		});
	});

	it("rejects a delta without a string item id", () => {
		expect(
			parseNotification({
				method: "item/agentMessage/delta",
				params: {
					threadId: "thread-1",
					turnId: "turn-1",
					itemId: 7,
					delta: "text",
				},
			}),
		).toBeNull();
	});

	it("parses an advisor dynamicToolCall item as an advisor note", () => {
		expect(
			parseNotification({
				method: "item/completed",
				params: {
					threadId: "thread-1",
					turnId: "turn-1",
					completedAtMs: 2,
					item: {
						type: "dynamicToolCall",
						id: "adv-1",
						tool: "advisor",
						namespace: null,
						arguments: {
							advisor: "advisor",
							severity: "blocker",
							delivery: "steer",
						},
						status: "completed",
						contentItems: [
							{ type: "inputText", text: "stop and run the tests" },
						],
						success: true,
						durationMs: 1,
					},
				},
			}),
		).toEqual({
			type: "itemCompleted",
			threadId: "thread-1",
			turnId: "turn-1",
			item: {
				id: "adv-1",
				kind: "advisorNote",
				advisor: "advisor",
				severity: "blocker",
				note: "stop and run the tests",
			},
		});
	});

	it("reads other dynamic tool output from contentItems", () => {
		const parsed = parseNotification({
			method: "item/completed",
			params: {
				threadId: "thread-1",
				completedAtMs: 2,
				turnId: "turn-1",
				item: {
					type: "dynamicToolCall",
					id: "tool-1",
					tool: "eval",
					arguments: {},
					status: "failed",
					contentItems: [{ type: "inputText", text: "boom" }],
				},
			},
		});
		expect(parsed).toMatchObject({
			item: {
				kind: "toolCall",
				name: "eval",
				output: "boom",
				status: "failed",
			},
		});
	});

	it("parses turn/plan/updated and rejects an unknown step status", () => {
		expect(
			parseNotification({
				method: "turn/plan/updated",
				params: {
					threadId: "thread-1",
					turnId: "turn-1",
					explanation: null,
					plan: [
						{ step: "read", status: "completed" },
						{ step: "fix", status: "inProgress" },
					],
				},
			}),
		).toEqual({
			type: "plan",
			threadId: "thread-1",
			turnId: "turn-1",
			explanation: null,
			steps: [
				{ step: "read", status: "completed" },
				{ step: "fix", status: "inProgress" },
			],
		});
		expect(() =>
			parseNotification({
				method: "turn/plan/updated",
				params: {
					threadId: "thread-1",
					turnId: "turn-1",
					plan: [{ step: "x", status: "done" }],
				},
			}),
		).toThrow("invalid plan step");
	});

	it("parses advisor status rows", () => {
		expect(
			parseAdvisorStatusResponse([
				{
					name: "advisor",
					model: "gpt",
					status: "reviewing",
					notes: 2,
					usage: { inputTokens: 1, outputTokens: 2 },
				},
			]),
		).toEqual([
			{ name: "advisor", model: "gpt", status: "reviewing", notes: 2 },
		]);
		expect(() =>
			parseAdvisorStatusResponse([{ name: "a", status: "asleep", notes: 0 }]),
		).toThrow("invalid advisor status");
	});

	it("keeps the JSON-RPC error code on rejected calls", () => {
		const error = new RpcError(-32001, "advisor runtime is not available");
		expect(error.code).toBe(-32001);
		expect(error.message).toBe(
			"rpc error -32001: advisor runtime is not available",
		);
	});

	it("reads the seq of a thread status and defaults it to zero", () => {
		expect(
			parseNotification({
				method: "thread/status/changed",
				params: { threadId: "thread-1", seq: 7, status: { type: "idle" } },
			}),
		).toMatchObject({ type: "threadStatus", threadId: "thread-1", seq: 7 });
		expect(
			parseNotification({
				method: "thread/status/changed",
				params: { threadId: "thread-1", status: { type: "idle" } },
			}),
		).toMatchObject({ type: "threadStatus", threadId: "thread-1", seq: 0 });
	});

	it("keeps the active flags of a thread status", () => {
		expect(
			parseNotification({
				method: "thread/status/changed",
				params: {
					threadId: "thread-1",
					seq: 8,
					status: { type: "active", activeFlags: ["waitingOnUserInput"] },
				},
			}),
		).toMatchObject({
			type: "threadStatus",
			status: "active",
			activeFlags: ["waitingOnUserInput"],
		});
	});

	it("reads the start of a turn in milliseconds", () => {
		expect(
			parseNotification({
				method: "turn/started",
				params: {
					threadId: "thread-1",
					turn: {
						id: "turn-1",
						status: "inProgress",
						items: [],
						error: null,
						startedAt: 1794391200,
					},
				},
			}),
		).toMatchObject({
			type: "turnStarted",
			turn: { id: "turn-1", startedAtMs: 1794391200000 },
		});
	});

	it("parses a retry wait and rejects one without a count", () => {
		expect(
			parseNotification({
				method: "dalgona/retry/wait",
				params: { threadId: "thread-1", turnId: "turn-1", remainingMs: 59000 },
			}),
		).toEqual({
			type: "retryWait",
			threadId: "thread-1",
			turnId: "turn-1",
			remainingMs: 59000,
		});
		expect(
			parseNotification({
				method: "dalgona/retry/wait",
				params: { threadId: "thread-1", turnId: "turn-1" },
			}),
		).toBeNull();
	});
});

describe("file change validation", () => {
	const completedFileChange = (changes: unknown) =>
		parseNotification({
			method: "item/completed",
			params: {
				threadId: "thread-1",
				turnId: "turn-1",
				completedAtMs: 3,
				item: {
					type: "fileChange",
					id: "fc-1",
					changes,
					status: "completed",
				},
			},
		});

	it("reads additions, deletions and diffOmitted of a file change", () => {
		const diff = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-one\n+uno\n";
		expect(
			completedFileChange([
				{
					path: "a.txt",
					kind: { type: "update" },
					diff,
					additions: 1,
					deletions: 1,
				},
			]),
		).toEqual({
			type: "itemCompleted",
			threadId: "thread-1",
			turnId: "turn-1",
			item: {
				id: "fc-1",
				kind: "fileChange",
				status: "completed",
				changes: [
					{
						path: "a.txt",
						kind: "update",
						diff,
						additions: 1,
						deletions: 1,
						diffOmitted: false,
					},
				],
				review: null,
			},
		});
	});

	it("leaves the counts unset when the server sends none", () => {
		const parsed = completedFileChange([
			{ path: "a.txt", kind: { type: "update" }, diff: "" },
		]);
		expect(parsed?.type === "itemCompleted" && parsed.item).toMatchObject({
			kind: "fileChange",
			changes: [{ path: "a.txt", kind: "update", diff: "" }],
		});
		if (parsed?.type !== "itemCompleted" || parsed.item.kind !== "fileChange") {
			throw new Error("expected a file change item");
		}
		expect(parsed.item.changes[0]).toStrictEqual({
			path: "a.txt",
			kind: "update",
			diff: "",
			diffOmitted: false,
		});
	});

	it("reads diffOmitted true with an empty diff and no counts", () => {
		const parsed = completedFileChange([
			{
				path: "big.bin",
				kind: { type: "update" },
				diff: "",
				diffOmitted: true,
			},
		]);
		if (parsed?.type !== "itemCompleted" || parsed.item.kind !== "fileChange") {
			throw new Error("expected a file change item");
		}
		expect(parsed.item.changes[0]).toStrictEqual({
			path: "big.bin",
			kind: "update",
			diff: "",
			diffOmitted: true,
		});
	});

	it("skips a file change whose counts are not integers", () => {
		expect(
			completedFileChange([
				{
					path: "a.txt",
					kind: { type: "update" },
					diff: "-one\n+uno\n",
					additions: "1",
					deletions: 1,
				},
			]),
		).toMatchObject({ item: { changes: [] } });
	});
});

describe("edit review validation", () => {
	it("parses dalgona/tool/judged into a tool verdict", () => {
		expect(
			parseNotification({
				method: "dalgona/tool/judged",
				params: {
					threadId: "thread-1",
					turnId: "turn-3",
					itemId: "call-7",
					verdict: "flagged",
					summary: "Flagged: removed code",
					paths: ["internal/model/retry.go"],
				},
			}),
		).toEqual({
			type: "toolJudged",
			threadId: "thread-1",
			itemId: "call-7",
			review: { verdict: "flagged", summary: "Flagged: removed code" },
		});
	});

	it("parses dalgona/turn/judged into a turn verdict", () => {
		expect(
			parseNotification({
				method: "dalgona/turn/judged",
				params: {
					threadId: "thread-1",
					turnId: "turn-3",
					verdict: "uncertain",
					summary: "Uncertain: incomplete",
					paths: [],
				},
			}),
		).toEqual({
			type: "turnJudged",
			threadId: "thread-1",
			turnId: "turn-3",
			review: { verdict: "uncertain", summary: "Uncertain: incomplete" },
		});
	});

	it("rejects an unknown verdict", () => {
		expect(
			parseNotification({
				method: "dalgona/tool/judged",
				params: {
					threadId: "thread-1",
					turnId: "turn-3",
					itemId: "call-7",
					verdict: "ok",
					summary: "ok",
					paths: [],
				},
			}),
		).toBeNull();
		expect(
			parseNotification({
				method: "dalgona/turn/judged",
				params: {
					threadId: "thread-1",
					turnId: "turn-3",
					verdict: "ok",
					summary: "ok",
					paths: [],
				},
			}),
		).toBeNull();
	});
});
