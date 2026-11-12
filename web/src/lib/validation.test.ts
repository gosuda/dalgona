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
			turn: { id: "turn-1", status: "completed", items: [], error: null },
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
});
