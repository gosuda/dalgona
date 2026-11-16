import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
	acceptThreadStatus,
	emptyTranscript,
	reduceNotification,
	requestsElsewhere,
	resolveRequest,
	trackRequest,
} from "#lib/reducer.ts";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type {
	AppNotification,
	ApprovalRequest,
	JsonValue,
	RpcHandlers,
	TranscriptItem,
	TranscriptState,
	TurnSnapshot,
} from "#lib/types.ts";

const location = {
	hash: "#token=abc",
	host: "localhost:7437",
	pathname: "/",
	protocol: "http:",
	search: "",
};

beforeEach(() => {
	vi.stubGlobal("history", { replaceState: vi.fn() });
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.useRealTimers();
});

function agentDelta(
	text: string,
	turnId: string = "turn-1",
	itemId: string = "item-agent",
): AppNotification {
	return {
		type: "delta",
		channel: "agent",
		threadId: "t1",
		turnId,
		itemId,
		delta: text,
	};
}

function userItem(id: string, text: string): TranscriptItem {
	return {
		id,
		kind: "userMessage",
		text,
		turnId: "turn-1",
		keywords: [],
	};
}

function turnOf(
	id: string,
	status: TurnSnapshot["status"],
	items: readonly TranscriptItem[],
): TurnSnapshot {
	return { id, status, items: [...items], error: null };
}

function runningTranscript(itemCount: number): TranscriptState {
	const items: TranscriptItem[] = [userItem("item-user", "hi")];
	for (let i = 0; i < itemCount; i += 1) {
		items.push({
			id: `item-a${i}`,
			kind: "agentMessage",
			text: "seed",
			complete: true,
			turnId: "turn-1",
		});
	}
	return {
		...emptyTranscript("t1"),
		turnId: "turn-1",
		turnStatus: "inProgress" as const,
		items,
	};
}

function deliver(
	port: StressPort,
	notifications: readonly AppNotification[],
): void {
	for (const notification of notifications) port.deliver(notification);
}

class StressPort implements RpcPort {
	private readonly handlers: RpcHandlers;
	private readonly table: Record<string, (params: JsonValue) => unknown>;
	private resolveOpen: (() => void) | null = null;
	private rejectOpen: ((error: Error) => void) | null = null;

	closed = false;
	notifications = 0;

	constructor(
		handlers: RpcHandlers,
		table: Record<string, (params: JsonValue) => unknown> = {},
	) {
		this.handlers = handlers;
		this.table = table;
	}

	open(): Promise<void> {
		this.handlers.onStateChange("connecting");
		const { promise, resolve, reject } = Promise.withResolvers<void>();
		this.resolveOpen = resolve;
		this.rejectOpen = reject;
		return promise;
	}

	failOpen(): void {
		this.handlers.onError("websocket error");
		this.handlers.onStateChange("disconnected");
		this.rejectOpen?.(new Error("connection closed"));
		this.resolveOpen = null;
		this.rejectOpen = null;
	}

	finishOpen(): void {
		this.handlers.onStateChange("connected");
		this.resolveOpen?.();
		this.resolveOpen = null;
		this.rejectOpen = null;
	}

	async call<T>(
		method: string,
		params: JsonValue,
		parse: (value: unknown) => T,
	): Promise<T> {
		const answer = this.table[method];
		if (answer === undefined) {
			throw new Error(`no answer for ${method}`);
		}
		return parse(answer(params));
	}

	notify(): void {}

	respond(): void {}

	close(): void {
		this.closed = true;
	}

	drop(): void {
		this.handlers.onStateChange("disconnected");
	}

	deliver(notification: AppNotification): void {
		this.notifications += 1;
		this.handlers.onNotification(notification);
	}

	request(approval: ApprovalRequest): void {
		this.handlers.onRequest(approval);
	}

	error(message: string): void {
		this.handlers.onError(message);
	}
}

function baseAnswers(): Record<string, (params: JsonValue) => unknown> {
	return {
		initialize: () => ({
			userAgent: "dalgona/0.1.0",
			platformFamily: "linux",
			platformOs: "linux",
		}),
		"thread/list": () => ({
			data: [
				{
					id: "t1",
					name: "one",
					preview: "",
					status: "idle",
					updatedAt: 1,
					cwd: "/work",
				},
				{
					id: "t2",
					name: "two",
					preview: "",
					status: "idle",
					updatedAt: 2,
					cwd: "/work",
				},
			],
			nextCursor: null,
		}),
		"model/list": () => ({ data: [] }),
		"config/read": () => ({
			config: { model: "gpt-test", origins: {} },
			origins: {},
		}),
		"dalgona/keywords/list": () => [],
		"dalgona/settings/read": () => ({ settings: [] }),
		"dalgona/advisor/status": () => [],
		"thread/resume": (params) => {
			const id =
				typeof params === "object" && params !== null && "threadId" in params
					? String(params["threadId"])
					: "t1";
			return {
				thread: {
					id,
					updatedAt: 1,
					status: "idle",
					cwd: "/work",
					turns: [
						{
							id: "turn-1",
							status: "inProgress",
							items: [
								{
									id: "item-user",
									type: "userMessage",
									content: [{ type: "text", text: "hi" }],
								},
							],
						},
					],
				},
				model: "gpt-test",
				approvalsReviewer: "user",
				modelProvider: "openai",
				approvalPolicy: "on-request",
				sandbox: "workspace-write",
				cwd: "/work",
			};
		},
	};
}

async function settle(turns = 80): Promise<void> {
	for (let i = 0; i < turns; i += 1) await Promise.resolve();
}

function connectedSession(
	overrides: Record<string, (params: JsonValue) => unknown> = {},
): { session: Session; ports: StressPort[] } {
	const ports: StressPort[] = [];
	const session = new Session((_url, handlers) => {
		const port = new StressPort(handlers, {
			...baseAnswers(),
			...overrides,
		});
		ports.push(port);
		return port;
	});
	session.start(location);
	return { session, ports };
}

async function connectedWithThread(
	overrides: Record<string, (params: JsonValue) => unknown> = {},
): Promise<{ session: Session; ports: StressPort[] }> {
	const made = connectedSession(overrides);
	await settle();
	made.ports[0]?.finishOpen();
	await settle();
	made.session.selectThread("t1");
	await settle();
	return made;
}

describe("delta storms", () => {
	test("thirty thousand deltas concatenate exactly once in order", () => {
		let state = runningTranscript(0);
		const chunks: string[] = [];
		for (let i = 0; i < 30000; i += 1) {
			const chunk = `${i},`;
			chunks.push(chunk);
			state = reduceNotification(state, agentDelta(chunk));
		}
		expect(state.items.length).toBe(2);
		const agent = state.items[1];
		expect(agent?.kind).toBe("agentMessage");
		expect(agent?.kind === "agentMessage" && agent.text).toBe(chunks.join(""));
	});

	test("ten thousand deltas over a large history keep the item list bounded", () => {
		let state = runningTranscript(2000);
		for (let i = 0; i < 10000; i += 1) {
			state = reduceNotification(state, agentDelta("x", "turn-1", "item-a0"));
		}
		expect(state.items.length).toBe(2001);
	});

	test("an out-of-order delta before itemStarted keeps the text once", () => {
		let state = runningTranscript(0);
		state = reduceNotification(state, agentDelta("tail"));
		state = reduceNotification(state, {
			type: "itemStarted",
			threadId: "t1",
			turnId: "turn-1",
			item: {
				id: "item-agent",
				kind: "agentMessage",
				text: "",
				complete: false,
			},
		});
		const agent = state.items.at(-1);
		expect(agent?.kind === "agentMessage" && agent.text).toBe("tail");
	});

	test("a duplicate itemStarted does not duplicate streamed text", () => {
		let state = runningTranscript(0);
		state = reduceNotification(state, {
			type: "itemStarted",
			threadId: "t1",
			turnId: "turn-1",
			item: {
				id: "item-agent",
				kind: "agentMessage",
				text: "",
				complete: false,
			},
		});
		state = reduceNotification(state, agentDelta("hello "));
		state = reduceNotification(state, {
			type: "itemStarted",
			threadId: "t1",
			turnId: "turn-1",
			item: {
				id: "item-agent",
				kind: "agentMessage",
				text: "",
				complete: false,
			},
		});
		const agent = state.items.at(-1);
		expect(agent?.kind === "agentMessage" && agent.text).toBe("hello ");
	});

	test("a duplicate itemCompleted is idempotent", () => {
		let state = runningTranscript(0);
		const completed: AppNotification = {
			type: "itemCompleted",
			threadId: "t1",
			turnId: "turn-1",
			item: {
				id: "item-agent",
				kind: "agentMessage",
				text: "done",
				complete: true,
			},
		};
		state = reduceNotification(state, completed);
		state = reduceNotification(state, completed);
		expect(state.items.length).toBe(2);
		const agent = state.items.at(-1);
		expect(agent?.kind === "agentMessage" && agent.text).toBe("done");
	});

	test("notifications for other threads change nothing", () => {
		const state = runningTranscript(0);
		const next = reduceNotification(state, {
			type: "delta",
			channel: "agent",
			threadId: "other",
			turnId: "turn-1",
			itemId: "item-agent",
			delta: "x",
		});
		expect(next).toBe(state);
	});
});

describe("turn status flicker", () => {
	test("turnStarted for a finished turn must not replace the status", () => {
		let state = runningTranscript(0);
		state = reduceNotification(state, {
			type: "turnCompleted",
			threadId: "t1",
			turn: turnOf("turn-1", "completed", []),
		});
		state = reduceNotification(state, {
			type: "turnStarted",
			threadId: "t1",
			turn: turnOf("turn-1", "inProgress", []),
		});
		expect(state.turnStatus).toBe("completed");
	});

	test("two thousand rapid toggles settle on the final status", () => {
		let state = runningTranscript(0);
		for (let i = 0; i < 2000; i += 1) {
			state = reduceNotification(state, {
				type: "turnStarted",
				threadId: "t1",
				turn: turnOf("turn-2", "inProgress", []),
			});
			state = reduceNotification(state, {
				type: "turnCompleted",
				threadId: "t1",
				turn: turnOf("turn-2", "completed", []),
			});
		}
		expect(state.turnStatus).toBe("completed");
		expect(state.error).toBeNull();
	});
});

describe("thread status seq", () => {
	test("a lower seq is ignored", () => {
		const applied = acceptThreadStatus({}, "t1", 10);
		expect(acceptThreadStatus(applied ?? {}, "t1", 3)).toBeNull();
	});

	test("a higher and an equal seq are accepted once each", () => {
		let applied = acceptThreadStatus({}, "t1", 5);
		applied = acceptThreadStatus(applied ?? {}, "t1", 5);
		expect(applied).toEqual({ t1: 5 });
		applied = acceptThreadStatus(applied ?? {}, "t1", 9);
		expect(applied).toEqual({ t1: 9 });
	});
});

describe("notice store bounds", () => {
	test("the store keeps at most five notices", () => {
		const { session, ports } = connectedSession();
		for (let i = 0; i < 30; i += 1) {
			session.notify("info", `notice ${i}`);
		}
		expect(session.notices.length).toBe(5);
		expect(session.notices[0]?.text).toBe("notice 25");
		ports[0]?.finishOpen();
	});

	test("a notice with the same text replaces the old one", () => {
		const { session, ports } = connectedSession();
		session.notify("info", "same", [{ label: "A", run: () => undefined }]);
		session.notify("info", "other");
		session.notify("warning", "same");
		expect(session.notices.length).toBe(2);
		expect(session.notices.at(-1)?.tone).toBe("warning");
		ports[0]?.finishOpen();
	});
});

describe("request store bounds", () => {
	test("duplicate request ids coalesce", () => {
		const approval = {
			id: "req-1",
			method: "item/commandExecution/requestApproval",
			threadId: "t1",
			turnId: "turn-1",
			itemId: "item-1",
			title: "Run",
			detail: "ls",
			reason: null,
		} as const;
		let requests = trackRequest([], approval);
		for (let i = 0; i < 5000; i += 1)
			requests = trackRequest(requests, approval);
		expect(requests.length).toBe(1);
		expect(resolveRequest(requests, "req-1").length).toBe(0);
	});

	test("four thousand distinct requests are tracked and scoped", () => {
		let requests = trackRequest([], {
			id: "req-0",
			method: "item/commandExecution/requestApproval",
			threadId: "t1",
			turnId: "turn-1",
			itemId: "item-0",
			title: "Run",
			detail: "ls",
			reason: null,
		});
		for (let i = 1; i < 4000; i += 1) {
			requests = trackRequest(requests, {
				id: `req-${i}`,
				method: "item/commandExecution/requestApproval",
				threadId: "t1",
				turnId: "turn-1",
				itemId: `item-${i}`,
				title: "Run",
				detail: "ls",
				reason: null,
			});
		}
		expect(requests.length).toBe(4000);
		expect(requestsElsewhere(requests, "t2", []).length).toBe(4000);
		expect(requestsElsewhere(requests, "t1", []).length).toBe(0);
	});
});

describe("stale connection deliveries", () => {
	test("notifications after a drop are ignored", async () => {
		vi.useFakeTimers();
		const made = await connectedWithThread();
		const { session, ports } = made;
		try {
			const port = ports[0];
			const itemsBefore = session.transcript.items.length;
			port?.drop();
			expect(session.connection).toBe("disconnected");
			const revisionAfterDrop = session.transcriptRevision;
			const noticesAfterDrop = session.notices;
			deliver(port as StressPort, [
				agentDelta("late", "turn-1", "item-user"),
				{
					type: "threadStatus",
					threadId: "t1",
					seq: 1,
					status: "active",
					activeFlags: [],
				},
				{
					type: "tokenUsage",
					threadId: "t1",
					turnId: "turn-1",
					usage: {
						last: {
							totalTokens: 1,
							inputTokens: 1,
							cachedInputTokens: 0,
							cacheWriteInputTokens: 0,
							outputTokens: 0,
							reasoningOutputTokens: 0,
						},
						total: {
							totalTokens: 1,
							inputTokens: 1,
							cachedInputTokens: 0,
							cacheWriteInputTokens: 0,
							outputTokens: 0,
							reasoningOutputTokens: 0,
						},
						contextWindow: 100,
						estimatedContextTokens: 1,
					},
				},
			]);
			port?.error("late error");
			expect(port?.notifications).toBe(3);
			expect(session.transcript.items.length).toBe(itemsBefore);
			expect(session.transcriptRevision).toBe(revisionAfterDrop);
			expect(session.notices).toBe(noticesAfterDrop);
			expect(session.notices.at(-1)?.text).toBe(
				`Reconnecting to dalgona (attempt 1 of 3).`,
			);
		} finally {
			session.stop();
		}
		await vi.advanceTimersByTimeAsync(1000);
		await settle();
		expect(ports.length).toBe(1);
	});

	test("notifications after stop are ignored", async () => {
		const made = await connectedWithThread();
		const { session, ports } = made;
		const port = ports[0];
		const itemsBefore = session.transcript.items.length;
		session.stop();
		deliver(port as StressPort, [agentDelta("late", "turn-1", "item-user")]);
		expect(session.transcript.items.length).toBe(itemsBefore);
	});
});

describe("reconnect storm", () => {
	test("rapid cycles leak no ports and no timers", async () => {
		vi.useFakeTimers();
		const made = connectedSession();
		const { session, ports } = made;
		await settle();
		ports[0]?.finishOpen();
		await settle();
		expect(session.connection).toBe("connected");
		for (let cycle = 0; cycle < 3; cycle += 1) {
			session.notify("info", "keep");
			ports.at(-1)?.drop();
			await settle();
			expect(session.connection).toBe("disconnected");
			expect(session.notices.at(-1)?.text).toBe(
				`Reconnecting to dalgona (attempt 1 of 3).`,
			);
			await vi.advanceTimersByTimeAsync(1000);
			await settle();
			ports.at(-1)?.finishOpen();
			await settle();
			expect(session.connection).toBe("connected");
			expect(session.notices.map((notice) => notice.text)).toEqual(["keep"]);
			await vi.advanceTimersByTimeAsync(20000);
			await settle();
		}
		const live = ports.at(-1);
		for (const port of ports) {
			if (port !== live) expect(port.closed).toBe(true);
		}
		ports.at(-1)?.drop();
		await settle();
		expect(live?.closed).toBe(true);
		for (let attempt = 1; attempt <= 3; attempt += 1) {
			await vi.advanceTimersByTimeAsync(1000 * 2 ** (attempt - 1));
			await settle();
			const port = ports.at(-1);
			expect(session.notices.at(-1)?.text).toBe(
				`Reconnecting to dalgona (attempt ${attempt} of 3).`,
			);
			port?.failOpen();
			await settle();
		}
		expect(session.notices.at(-1)?.text).toBe("Disconnected from dalgona.");
		expect(vi.getTimerCount()).toBe(0);
		session.connect();
		await settle();
		ports.at(-1)?.finishOpen();
		await settle();
		expect(session.connection).toBe("connected");
		expect(session.notices.map((notice) => notice.text)).toEqual(["keep"]);
		session.stop();
		expect(vi.getTimerCount()).toBe(0);
		vi.useRealTimers();
	});

	test("every port but the live one is closed after the storm", async () => {
		vi.useFakeTimers();
		const made = connectedSession();
		const { session, ports } = made;
		await settle();
		ports[0]?.finishOpen();
		await settle();
		for (let cycle = 0; cycle < 4; cycle += 1) {
			ports.at(-1)?.drop();
			await vi.advanceTimersByTimeAsync(1000);
			await settle();
			ports.at(-1)?.finishOpen();
			await settle();
			expect(session.connection).toBe("connected");
			await vi.advanceTimersByTimeAsync(20000);
			await settle();
		}
		const live = ports.at(-1);
		for (const port of ports) {
			if (port !== live) expect(port.closed).toBe(true);
		}
		expect(live?.closed).toBe(false);
		session.stop();
		await settle();
		expect(live?.closed).toBe(true);
		vi.useRealTimers();
	});
});

describe("usage and thread stores", () => {
	test("twenty thousand token usage updates keep one entry per thread", async () => {
		const made = await connectedWithThread();
		const { session, ports } = made;
		const usage = {
			last: {
				totalTokens: 1,
				inputTokens: 1,
				cachedInputTokens: 0,
				cacheWriteInputTokens: 0,
				outputTokens: 0,
				reasoningOutputTokens: 0,
			},
			total: {
				totalTokens: 1,
				inputTokens: 1,
				cachedInputTokens: 0,
				cacheWriteInputTokens: 0,
				outputTokens: 0,
				reasoningOutputTokens: 0,
			},
			contextWindow: 100,
			estimatedContextTokens: 1,
		};
		const storm: AppNotification[] = [];
		for (let i = 0; i < 20000; i += 1) {
			storm.push({
				type: "tokenUsage",
				threadId: i % 2 === 0 ? "t1" : "t2",
				turnId: "turn-1",
				usage,
			});
		}
		deliver(ports[0] as StressPort, storm);
		expect(Object.keys(session.usage).length).toBe(2);
	});

	test("twenty thousand status updates keep the thread list at two", async () => {
		const made = await connectedWithThread();
		const { session, ports } = made;
		const storm: AppNotification[] = [];
		for (let i = 0; i < 20000; i += 1) {
			storm.push({
				type: "threadStatus",
				threadId: "t1",
				seq: i,
				status: i % 2 === 0 ? "active" : "idle",
				activeFlags: [],
			});
		}
		deliver(ports[0] as StressPort, storm);
		expect(session.threads.length).toBe(2);
		expect(session.threads[0]?.status).toBe("idle");
	});
});

describe("approval storms", () => {
	test("four thousand approvals track and then resolve", async () => {
		const made = await connectedWithThread();
		const { session, ports } = made;
		const port = ports[0] as StressPort;
		const storm: ApprovalRequest[] = Array.from({ length: 4000 }, (_, i) => ({
			id: `req-${i}`,
			method: "item/commandExecution/requestApproval",
			threadId: "t1",
			turnId: "turn-1",
			itemId: `item-${i}`,
			title: "Run",
			detail: "ls",
			reason: null,
		}));
		for (const approval of storm) port.request(approval);
		expect(session.requests.length).toBe(4000);
		expect(session.currentRequest?.id).toBe("req-0");
		deliver(
			port,
			storm.map((approval) => ({
				type: "requestResolved",
				requestId: approval.id,
				resolution: null,
			})),
		);
		expect(session.requests.length).toBe(0);
		expect(session.currentRequest).toBeNull();
		expect(session.notices.length).toBe(0);
	});
});

describe("history under churn", () => {
	test("deltas do not disturb history and new user messages join it", async () => {
		const made = await connectedWithThread({
			"turn/start": () => ({
				turn: turnOf("turn-next", "inProgress", [userItem("item-new", "one")]),
			}),
			"turn/steer": () => ({ turnId: "turn-next" }),
		});
		const { session, ports } = made;
		const port = ports[0];
		await session.sendMessage("one");
		expect(session.history).toEqual(["hi", "one"]);
		const storm: AppNotification[] = [];
		for (let i = 0; i < 5000; i += 1) {
			storm.push(agentDelta("x", "turn-next", "item-new"));
		}
		deliver(port as StressPort, storm);
		expect(session.history).toEqual(["hi", "one"]);
		deliver(port as StressPort, [
			{
				type: "itemStarted",
				threadId: "t1",
				turnId: "turn-next",
				item: userItem("item-user-2", "two"),
			},
		]);
		expect(session.history).toEqual(["hi", "two", "one"]);
		await session.steerMessage("three");
		expect(session.history).toEqual(["hi", "two", "one", "three"]);
	});

	test("switching threads rebuilds history from that thread", async () => {
		const made = await connectedWithThread({
			"thread/resume": (params) => {
				const id =
					typeof params === "object" && params !== null && "threadId" in params
						? String(params["threadId"])
						: "t1";
				const text = id === "t2" ? "from two" : "hi";
				return {
					thread: {
						id,
						updatedAt: 1,
						status: "idle",
						cwd: "/work",
						turns: [
							{
								id: "turn-1",
								status: "completed",
								items: [
									{
										id: "item-user",
										type: "userMessage",
										content: [{ type: "text", text }],
									},
								],
							},
						],
					},
					model: "gpt-test",
					approvalsReviewer: "user",
					modelProvider: "openai",
					approvalPolicy: "on-request",
					sandbox: "workspace-write",
					cwd: "/work",
				};
			},
		});
		const { session } = made;
		expect(session.history).toEqual(["hi"]);
		await session.selectThread("t2");
		expect(session.history).toEqual(["from two"]);
	});
});
