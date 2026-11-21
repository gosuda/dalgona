import {
	afterEach,
	beforeEach,
	describe,
	expect,
	type Mock,
	test,
	vi,
} from "vitest";
import { RpcError } from "./protocol";
import { type RpcPort, Session } from "./session.svelte";
import type {
	AppNotification,
	ApprovalRequest,
	JsonValue,
	QuestionRequest,
	RequestId,
	RpcHandlers,
	ThreadListResult,
} from "./types";
import { QUESTION_METHOD } from "./types";
import { parseNotification, parseThreadStartResponse } from "./validation";

type Answer = (params: JsonValue) => JsonValue;

const location = {
	hash: "#token=abc",
	host: "localhost:7437",
	pathname: "/",
	protocol: "http:",
	search: "",
};

function threadIdOf(params: JsonValue): string {
	if (
		typeof params !== "object" ||
		params === null ||
		!("threadId" in params)
	) {
		return "";
	}
	const { threadId: id } = params;
	return typeof id === "string" ? id : "";
}

function thread(id: string, turns: JsonValue): Record<string, JsonValue> {
	return {
		id,
		updatedAt: 1,
		status: { type: "idle" },
		turns,
		cwd: "/work",
	};
}

function threadListPage(
	params: JsonValue,
	rows: readonly JsonValue[],
): JsonValue {
	const offset =
		typeof params === "object" && params !== null && "cursor" in params
			? Number(params["cursor"])
			: 0;
	const end = offset + 100;
	return {
		data: rows.slice(offset, end),
		nextCursor: end < rows.length ? String(end) : null,
	};
}

function resumeAnswer(
	runningThread: string,
): (params: JsonValue) => Record<string, JsonValue> {
	return (params) => {
		const id = threadIdOf(params);
		const turns: JsonValue =
			id === runningThread
				? [{ id: "turn-1", status: "inProgress", items: [] }]
				: [];
		return {
			thread: thread(id, turns),
			model: "gpt-test",
			approvalsReviewer: "user",
			modelProvider: "openai",
			approvalPolicy: "on-request",
			sandbox: "workspace-write",
			cwd: "/work",
		};
	};
}

function answers(runningThread: string): Record<string, Answer> {
	return {
		initialize: () => ({
			userAgent: "dalgona/0.1.0",
			platformFamily: "linux",
			platformOs: "linux",
		}),
		"thread/list": () => ({
			data: [thread("t1", []), thread("t2", [])],
			nextCursor: null,
		}),
		"model/list": () => ({ data: [] }),
		"config/read": () => ({ config: { model: "gpt-test" }, origins: {} }),
		"dalgona/keywords/list": () => [],
		"thread/resume": resumeAnswer(runningThread),
		"thread/read": (params) => ({ thread: thread(threadIdOf(params), []) }),
		"dalgona/advisor/status": () => [],
		"dalgona/settings/read": () => ({
			settings: [
				{
					key: "theme",
					value: "soft-night",
					defaultValue: "auto",
					options: [
						"auto",
						"auto-soft",
						"night",
						"soft-night",
						"day",
						"soft-day",
					],
				},
			],
		}),
	};
}

type OpenMode = "open" | "refuse" | "hang";

class FakePort implements RpcPort {
	private readonly log: string[] = [];
	private readonly handlers: RpcHandlers;
	private readonly table: Record<string, Answer>;
	private readonly mode: OpenMode;

	constructor(
		handlers: RpcHandlers,
		table: Record<string, Answer>,
		mode: OpenMode = "open",
	) {
		this.handlers = handlers;
		this.table = table;
		this.mode = mode;
	}

	get calls(): readonly string[] {
		return this.log;
	}

	open(): Promise<void> {
		if (this.mode === "hang") return Promise.withResolvers<void>().promise;
		if (this.mode === "refuse") {
			this.handlers.onStateChange("connecting");
			this.handlers.onError("websocket error");
			this.handlers.onStateChange("disconnected");
			return Promise.reject(new Error("connection closed"));
		}
		this.handlers.onStateChange("connected");
		return Promise.resolve();
	}

	call<T>(
		method: string,
		params: JsonValue,
		parse: (value: unknown) => T,
	): Promise<T> {
		this.log.push(method);
		const answer = this.table[method];
		if (answer === undefined) {
			return Promise.reject(new Error(`no answer for ${method}`));
		}
		return Promise.resolve(parse(answer(params)));
	}

	notify(_method: string, _params: JsonValue): void {}

	deliver(notification: AppNotification): void {
		this.handlers.onNotification(notification);
	}
	request(request: ApprovalRequest): void {
		this.handlers.onRequest(request);
	}
	question(request: QuestionRequest): void {
		this.handlers.onQuestion(request);
	}
	respond(_id: RequestId, _result: JsonValue): void {}

	close(): void {}

	drop(): void {
		this.handlers.onStateChange("disconnected");
	}
}

function fakeDial(
	runningThread: string,
	overrides: Record<string, Answer> = {},
	mode: (index: number) => OpenMode = () => "open",
): {
	dial: (url: string, handlers: RpcHandlers) => RpcPort;
	ports: FakePort[];
} {
	const ports: FakePort[] = [];
	const dial = (_url: string, handlers: RpcHandlers): RpcPort => {
		const port = new FakePort(
			handlers,
			{
				...answers(runningThread),
				...overrides,
			},
			mode(ports.length),
		);
		ports.push(port);
		return port;
	};
	return { dial, ports };
}

async function waitForThread(
	session: Session,
	threadId: string,
): Promise<void> {
	await vi.waitFor(() => {
		expect(session.activeThreadId).toBe(threadId);
		expect(session.loadingTranscript).toBe(false);
	});
}

async function openThread(session: Session, threadId: string): Promise<void> {
	await vi.waitFor(() => {
		expect(session.loadingThreads).toBe(false);
		expect(session.threads.some((entry) => entry.id === threadId)).toBe(true);
	});
	await session.selectThread(threadId);
	await waitForThread(session, threadId);
}

function configFixture(): { read: Answer; write: Answer } {
	let model = "gpt-config";
	let effort: string | null = "low";
	const read: Answer = () => ({
		config: {
			model,
			approval_policy: "never",
			sandbox_mode: "workspace-write",
			model_reasoning_effort: effort,
		},
		origins: {},
	});
	const write: Answer = (params) => {
		const edits =
			typeof params === "object" && params !== null && "edits" in params
				? params["edits"]
				: null;
		if (Array.isArray(edits)) {
			for (const edit of edits) {
				if (typeof edit !== "object" || edit === null) continue;
				if (!("keyPath" in edit) || !("value" in edit)) continue;
				if (edit.keyPath === "model" && typeof edit.value === "string") {
					model = edit.value;
				}
				if (edit.keyPath === "model_reasoning_effort") {
					effort = typeof edit.value === "string" ? edit.value : null;
				}
			}
		}
		return {};
	};
	return { read, write };
}

function effectiveStatusResume(params: JsonValue): JsonValue {
	const id = threadIdOf(params);
	if (id === "t2") {
		return {
			thread: thread("t2", []),
			model: "gpt-t2",
			approvalsReviewer: "user",
			modelProvider: "openai",
			approvalPolicy: "on-request",
			sandbox: "read-only",
			reasoningEffort: "low",
			cwd: "/work",
		};
	}
	return {
		thread: thread(id, []),
		model: "gpt-thread",
		approvalsReviewer: "user",
		modelProvider: "openai",
		approvalPolicy: "untrusted",
		sandbox: "dangerFullAccess",
		reasoningEffort: "medium",
		cwd: "/work",
	};
}

function statusNotice(seq: number, status: string): AppNotification {
	const parsed = parseNotification({
		method: "thread/status/changed",
		params: { threadId: "t1", seq, status: { type: status } },
	});
	if (parsed === null) throw new Error("status notice did not parse");
	return parsed;
}

function patchNotice(
	threadId: string,
	itemId: string,
	path: string,
): AppNotification {
	const parsed = parseNotification({
		method: "item/fileChange/patchUpdated",
		params: {
			threadId,
			turnId: "turn-1",
			itemId,
			changes: [
				{
					path,
					kind: { type: "update" },
					diff: `--- a/${path}\n+++ b/${path}\n@@ -1 +1 @@\n-old\n+new\n`,
					additions: 1,
					deletions: 1,
				},
			],
		},
	});
	if (parsed === null) throw new Error("patch notice did not parse");
	return parsed;
}

function resolvedNotice(
	threadId: string,
	requestId: string,
	resolution?: string,
): AppNotification {
	const parsed = parseNotification({
		method: "serverRequest/resolved",
		params:
			resolution === undefined
				? { threadId, requestId }
				: { threadId, requestId, resolution },
	});
	if (parsed === null) throw new Error("resolved notice did not parse");
	return parsed;
}

function userItem(id: string, text: string): AppNotification {
	const parsed = parseNotification({
		method: "item/completed",
		params: {
			completedAtMs: 1,
			item: { id, type: "userMessage", content: [{ type: "text", text }] },
			threadId: "t1",
			turnId: "turn-1",
		},
	});
	if (parsed === null) throw new Error("user item did not parse");
	return parsed;
}

function fileApproval(
	id: string,
	threadId: string,
	itemId: string,
): ApprovalRequest {
	return {
		id,
		method: "item/fileChange/requestApproval",
		threadId,
		turnId: "turn-1",
		itemId,
		title: "Apply file changes?",
		detail: "",
		reason: null,
	};
}

function paths(changes: readonly { readonly path: string }[]): string[] {
	return changes.map((change) => change.path);
}

describe("session", () => {
	beforeEach(() => {
		vi.stubGlobal("history", { replaceState: vi.fn() });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	test("applies the theme the server reports", async () => {
		const applied: string[] = [];
		const { dial } = fakeDial("t1");
		const session = new Session(dial, (setting) => applied.push(setting));
		session.start(location);
		await vi.waitFor(() => {
			expect(applied).toEqual(["soft-night"]);
		});
		session.stop();
	});

	test("loads the list and opens no thread", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await vi.waitFor(() => {
			expect(session.threads.map((entry) => entry.id)).toEqual(["t1", "t2"]);
			expect(ports[0]?.calls).toContain("dalgona/settings/read");
		});
		expect(session.activeThreadId).toBeNull();
		expect(session.loadingTranscript).toBe(false);
		expect(ports[0]?.calls).not.toContain("thread/resume");
		expect(ports[0]?.calls).not.toContain("thread/read");
		session.stop();
	});

	test("reopens the stored active thread", async () => {
		const resumed: string[] = [];
		const { dial, ports } = fakeDial("none", {
			"thread/resume": (params) => {
				resumed.push(threadIdOf(params));
				return resumeAnswer("none")(params);
			},
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t2");
		expect(resumed).toEqual(["t2"]);

		session.start(location);
		await vi.waitFor(() => {
			expect(ports).toHaveLength(2);
			expect(resumed).toEqual(["t2", "t2"]);
		});
		await waitForThread(session, "t2");
		session.stop();
	});

	test("requests the thread list with only its limit", async () => {
		const requests: JsonValue[] = [];
		const { dial } = fakeDial("none", {
			"thread/list": (params) => {
				requests.push(params);
				return { data: [thread("t1", [])], nextCursor: null };
			},
		});
		const session = new Session(dial);
		session.start(location);
		await vi.waitFor(() => {
			expect(session.threads).toHaveLength(1);
		});
		expect(requests).toEqual([{ limit: 100 }]);
		expect(requests[0]).not.toHaveProperty("sortKey");
		expect(requests[0]).not.toHaveProperty("sortDirection");
		session.stop();
	});

	test.each(["notice", "sidebar"])(
		"clears the page error after a retry from the %s",
		async (source) => {
			const requests: JsonValue[] = [];
			const { dial, ports } = fakeDial("t1", {
				"thread/list": (params) => {
					requests.push(params);
					if (requests.length === 1)
						return { data: [thread("t1", [])], nextCursor: "older" };
					if (requests.length === 2) throw new Error("temporarily unavailable");
					return { data: [thread("t2", [])], nextCursor: "oldest" };
				},
			});
			const session = new Session(dial);
			session.start(location);
			await openThread(session, "t1");
			await session.loadMoreThreads();
			expect(session.threads.map((entry) => entry.id)).toEqual(["t1"]);
			const retry = session.notices.find((notice) =>
				notice.text.includes("temporarily unavailable"),
			)?.actions[0];
			expect(retry?.label).toBe("Try again");
			if (source === "notice") retry?.run();
			else await session.loadMoreThreads();
			await vi.waitFor(() => expect(session.nextThreadCursor).toBe("oldest"));
			expect(
				session.notices.some((notice) =>
					notice.text.includes("temporarily unavailable"),
				),
			).toBe(false);
			retry?.run();
			expect(requests).toEqual([
				{ limit: 100 },
				{ limit: 100, cursor: "older" },
				{ limit: 100, cursor: "older" },
			]);
			expect(session.threads.map((entry) => entry.id)).toEqual(["t1", "t2"]);
			expect(
				ports[0]?.calls.filter((method) => method === "thread/resume"),
			).toHaveLength(1);
			expect(session.activeThreadId).toBe("t1");
			session.stop();
		},
	);

	test.each(["overlap", "last page", "updated first page"])(
		"finds a thread promoted ahead of two loaded pages on %s",
		async (boundary) => {
			let ids = Array.from({ length: 302 }, (_, index) => String(index + 1));
			const updated = new Set<string>();
			const { dial } = fakeDial("none", {
				"thread/list": (params) =>
					threadListPage(
						params,
						ids.map((id) => ({
							...thread(id, []),
							updatedAt: updated.has(id) ? 2 : 1,
						})),
					),
			});
			const session = new Session(dial);
			session.start(location);
			await vi.waitFor(() => expect(session.nextThreadCursor).toBe("100"));
			await session.loadMoreThreads();
			if (boundary === "updated first page") {
				const head = ids.slice(0, 100);
				for (const id of head) updated.add(id);
				ids = [...head, "302", ...ids.slice(101, -1)];
			} else {
				ids = ["302", ...ids.slice(0, -1)];
				if (boundary === "last page") ids = ids.filter((id) => id !== "1");
			}
			await session.loadMoreThreads();
			if (boundary === "overlap")
				expect(session.threads.some((entry) => entry.id === "302")).toBe(true);
			expect(session.nextThreadCursor).toBe("300");
			await session.loadMoreThreads();
			const visible = new Set(session.threads.map((entry) => entry.id));
			expect(ids.filter((id) => !visible.has(id))).toEqual([]);
			expect(session.threads).toHaveLength(visible.size);
			expect(session.nextThreadCursor).toBeNull();
			session.stop();
		},
	);

	test("checks only the first page when an unchanged history reaches its last page", async () => {
		const rows = Array.from({ length: 1001 }, (_, index) =>
			thread(String(index + 1), []),
		);
		const requests: JsonValue[] = [];
		const { dial } = fakeDial("none", {
			"thread/list": (params) => {
				requests.push(params);
				return threadListPage(params, rows);
			},
		});
		const session = new Session(dial);
		session.start(location);
		await vi.waitFor(() => expect(session.nextThreadCursor).toBe("100"));
		for (let offset = 100; offset < 1000; offset += 100)
			await session.loadMoreThreads();
		expect(requests).toHaveLength(10);
		requests.length = 0;
		await session.loadMoreThreads();
		expect(requests).toEqual([{ limit: 100, cursor: "1000" }, { limit: 100 }]);
		expect(session.threads).toHaveLength(1001);
		expect(session.nextThreadCursor).toBeNull();
		session.stop();
	});

	test("discards an older page that arrives after reconnect", async () => {
		const { dial } = fakeDial("none", {
			"thread/list": () => ({ data: [thread("t1", [])], nextCursor: "older" }),
		});
		const session = new Session(dial);
		session.start(location);
		await vi.waitFor(() => expect(session.nextThreadCursor).toBe("older"));
		const pending = Promise.withResolvers<ThreadListResult>();
		vi.spyOn(session, "listThreads").mockReturnValueOnce(pending.promise);
		const loading = session.loadMoreThreads();
		await session.connect();
		pending.resolve({ threads: [], nextCursor: "stale" });
		await loading;
		expect(session.nextThreadCursor).toBe("older");
		session.stop();
	});

	test("reconnect keeps queued follow-ups", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		expect(session.running).toBe(true);
		session.queueFollowup("later");
		expect(session.queuedFollowups).toEqual(["later"]);

		session.start(location);
		await vi.waitFor(() => {
			expect(ports).toHaveLength(2);
			expect(ports[1]?.calls).toContain("thread/resume");
		});
		await waitForThread(session, "t1");
		expect(session.queuedFollowups).toEqual(["later"]);
		session.stop();
	});

	test("lists each sent message once in the history", async () => {
		const { dial, ports } = fakeDial("none", {
			"turn/start": () => ({
				turn: { id: "turn-1", status: "inProgress", items: [] },
			}),
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		await session.sendMessage("first");
		await session.sendMessage("second");
		ports[0]?.deliver(userItem("u-1", "first"));
		ports[0]?.deliver(userItem("u-2", "second"));
		expect(session.history).toEqual(["first", "second"]);
		session.stop();
	});

	test("editing a follow-up returns its text and drops only that entry", async () => {
		const { dial } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		for (const text of ["a", "b", "c"]) session.queueFollowup(text);
		expect(session.editFollowup(1)).toBe("b");
		expect(session.queuedFollowups).toEqual(["a", "c"]);
		session.stop();
	});

	test("removing a follow-up drops only that entry", async () => {
		const { dial } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		for (const text of ["a", "b", "c"]) session.queueFollowup(text);
		session.removeFollowup(1);
		expect(session.queuedFollowups).toEqual(["a", "c"]);
		session.stop();
	});

	test("opening another thread discards queued follow-ups", async () => {
		const { dial } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		session.queueFollowup("later");
		expect(session.queuedFollowups).toEqual(["later"]);

		await session.selectThread("t2");
		expect(session.activeThreadId).toBe("t2");
		expect(session.queuedFollowups).toEqual([]);
		session.stop();
	});

	test("clears the open thread and transcript when resume finds no thread", async () => {
		const { dial } = fakeDial("none", {
			"thread/resume": (params) => {
				if (threadIdOf(params) === "t2")
					throw new RpcError(-32602, "unknown thread t2");
				return {
					...resumeAnswer("none")(params),
					thread: thread("t1", [
						{
							id: "turn-1",
							status: "completed",
							items: [
								{
									id: "i1",
									type: "userMessage",
									content: [{ type: "text", text: "hi" }],
								},
							],
						},
					]),
				};
			},
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		expect(session.transcript.items).toHaveLength(1);
		await session.selectThread("t2");
		await vi.waitFor(() => {
			expect(session.activeThreadId).toBeNull();
			expect(session.transcript.threadId).toBeNull();
			expect(session.transcript.items).toEqual([]);
		});
		session.stop();
	});

	test("forgets a file change's diff once its approval resolves", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		const port = ports[0];
		if (port === undefined) throw new Error("no port");
		port.deliver(patchNotice("t1", "fc-9", "a.txt"));
		port.request(fileApproval("req-1", "t1", "fc-9"));
		expect(paths(session.approvalChanges)).toEqual(["a.txt"]);
		port.deliver(resolvedNotice("t1", "req-1"));
		port.request(fileApproval("req-2", "t1", "fc-9"));
		expect(paths(session.approvalChanges)).toEqual([]);
		session.stop();
	});

	test("marks the transcript row Timed out when the question times out", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		const port = ports[0];
		if (port === undefined) throw new Error("no port");
		port.question(question("req-1"));
		expect(session.requests).toHaveLength(1);
		port.deliver(resolvedNotice("t1", "req-1", "timedOut"));
		expect(session.requests).toHaveLength(0);
		expect([...session.timedOutQuestions]).toEqual(["call-1"]);
		expect(session.notices.at(-1)?.text).toBe("Question timed out");
		port.question(question("req-2"));
		port.deliver(resolvedNotice("t1", "req-2", "answered"));
		expect([...session.timedOutQuestions]).toEqual(["call-1"]);
		session.stop();
	});

	test("keeps a pending approval's diff across a thread switch", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		const port = ports[0];
		if (port === undefined) throw new Error("no port");
		port.deliver(patchNotice("t1", "fc-9", "a.txt"));
		port.request(fileApproval("req-1", "t1", "fc-9"));
		await session.selectThread("t2");
		await session.selectThread("t1");
		expect(paths(session.approvalChanges)).toEqual(["a.txt"]);
		session.stop();
	});

	test("drops a file change's diff that no pending approval needs on a thread switch", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		const port = ports[0];
		if (port === undefined) throw new Error("no port");
		port.deliver(patchNotice("t1", "fc-7", "stale.txt"));
		await session.selectThread("t2");
		await session.selectThread("t1");
		port.request(fileApproval("req-7", "t1", "fc-7"));
		expect(paths(session.approvalChanges)).toEqual([]);
		session.stop();
	});

	test("does not apply a thread status that arrives after a newer one", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		const port = ports[0];
		if (port === undefined) throw new Error("no port was dialled");
		port.deliver(statusNotice(5, "active"));
		port.deliver(statusNotice(3, "idle"));
		expect(session.threads.find((thread) => thread.id === "t1")?.status).toBe(
			"active",
		);
		session.stop();
	});

	test("keeps one notice per text and at most five", () => {
		const { dial } = fakeDial("t1");
		const session = new Session(dial);
		session.notify("error", "Same");
		session.notify("warning", "Same");
		expect(session.notices.map((notice) => notice.tone)).toEqual(["warning"]);
		for (let index = 0; index < 6; index += 1) {
			session.notify("info", `note ${index}`);
		}
		expect(session.notices.map((notice) => notice.text)).toEqual([
			"note 1",
			"note 2",
			"note 3",
			"note 4",
			"note 5",
		]);
	});

	test("tracks the rate-limit wait of the open turn until it reaches zero", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		const wait = (remainingMs: number): AppNotification => {
			const parsed = parseNotification({
				method: "dalgona/retry/wait",
				params: { threadId: "t1", turnId: "turn-1", remainingMs },
			});
			if (parsed === null) throw new Error("retry wait did not parse");
			return parsed;
		};
		ports[0]?.deliver(wait(90000));
		expect(session.retryRemainingMs).toBe(90000);
		ports[0]?.deliver(wait(0));
		expect(session.retryRemainingMs).toBeNull();
		session.stop();
	});

	test("saving a model keeps the effective sandbox", async () => {
		{
			const config = configFixture();
			const { dial } = fakeDial("t1", {
				"config/read": config.read,
				"config/batchWrite": config.write,
				"thread/resume": effectiveStatusResume,
			});
			const session = new Session(dial);
			session.start(location);
			await openThread(session, "t1");
			await session.saveModel("gpt-saved", "high");
			expect(session.status).toEqual({
				model: "gpt-saved",
				approval: "ask",
				sandbox: "off",
				effort: "high",
			});
			session.stop();
		}

		{
			const config = configFixture();
			const { dial } = fakeDial("none", {
				"config/read": config.read,
				"config/batchWrite": config.write,
				"thread/list": () => ({ data: [], nextCursor: null }),
			});
			const session = new Session(dial);
			session.start(location);
			await vi.waitFor(() => {
				expect(session.status.model).toBe("gpt-config");
			});
			await session.saveModel("gpt-bare", "high");
			expect(session.status).toEqual({
				model: "gpt-bare",
				approval: "yolo",
				sandbox: "workspace-write",
				effort: "high",
			});
			session.stop();
		}

		{
			const config = configFixture();
			let failRead = false;
			const { dial } = fakeDial("t1", {
				"config/read": (params) => {
					if (failRead) {
						failRead = false;
						throw new Error("config read failed");
					}
					return config.read(params);
				},
				"config/batchWrite": config.write,
				"thread/resume": effectiveStatusResume,
			});
			const session = new Session(dial);
			session.start(location);
			await openThread(session, "t1");
			failRead = true;
			await session.saveModel("gpt-retry", "high");
			const notice = session.notices.find((item) =>
				item.text.startsWith("Unable to load configuration"),
			);
			const retry = notice?.actions.find(
				(action) => action.label === "Try again",
			);
			expect(retry).toBeDefined();
			retry?.run();
			await vi.waitFor(() => {
				expect(session.status.model).toBe("gpt-retry");
			});
			expect(session.status).toEqual({
				model: "gpt-retry",
				approval: "ask",
				sandbox: "off",
				effort: "high",
			});
			session.stop();
		}

		{
			const config = configFixture();
			const { dial, ports } = fakeDial("t1", {
				"config/read": config.read,
				"config/batchWrite": config.write,
				"thread/resume": effectiveStatusResume,
			});
			const session = new Session(dial);
			session.start(location);
			await openThread(session, "t1");
			const port = ports[0];
			if (port === undefined) throw new Error("no port");
			let release = (): void => {};
			const gate = new Promise<void>((resolve) => {
				release = resolve;
			});
			holdMethod(port, "config/read", gate);
			const saving = session.saveModel("gpt-held", "high");
			await session.selectThread("t2");
			release();
			expect(await saving).toBe(true);
			expect(session.status).toEqual({
				model: "gpt-held",
				approval: "auto",
				sandbox: "read-only",
				effort: "high",
			});
			session.stop();
		}

		{
			const config = configFixture();
			const { dial, ports } = fakeDial("t1", {
				"config/read": config.read,
				"config/batchWrite": config.write,
				"thread/resume": effectiveStatusResume,
			});
			const session = new Session(dial);
			session.start(location);
			await openThread(session, "t1");
			const port = ports[0];
			if (port === undefined) throw new Error("no port");
			let release = (): void => {};
			const gate = new Promise<void>((resolve) => {
				release = resolve;
			});
			holdMethod(port, "config/read", gate);
			const saving = session.saveModel("gpt-again", "high");
			session.start(location);
			await vi.waitFor(() => {
				expect(ports).toHaveLength(2);
				expect(ports[1]?.calls).toContain("thread/read");
				expect(session.loadingTranscript).toBe(false);
			});
			const status = session.status;
			const selected = session.selectedModel;
			release();
			expect(await saving).toBe(true);
			expect(session.status).toEqual(status);
			expect(session.selectedModel).toBe(selected);
			session.stop();
		}
	});
});

describe("recovery actions", () => {
	beforeEach(() => {
		vi.stubGlobal("history", { replaceState: vi.fn() });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	test("offers Try again for a failed request while connected", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		void session.saveModel("gpt-next", null);
		await vi.waitFor(() => {
			expect(session.notices.map((notice) => notice.text)).toContain(
				"Unable to save model selection: no answer for config/batchWrite.",
			);
		});
		const notice = session.notices.find((item) =>
			item.text.startsWith("Unable to save model selection"),
		);
		const again = notice?.actions.find(
			(action) => action.label === "Try again",
		);
		expect(again).toBeDefined();
		again?.run();
		expect(
			ports[0]?.calls.filter((method) => method === "config/batchWrite"),
		).toHaveLength(2);
		session.stop();
	});

	test("offers Reconnect when the connection drops", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		ports[0]?.drop();
		const notice = session.notices.find(
			(item) => item.text === "Reconnecting to dalgona (attempt 1 of 3).",
		);
		const reconnect = notice?.actions.find(
			(action) => action.label === "Reconnect",
		);
		expect(reconnect).toBeDefined();
		reconnect?.run();
		await vi.waitFor(() => {
			expect(ports).toHaveLength(2);
		});
		session.stop();
	});
});

function noticeTexts(session: Session): string[] {
	return session.notices.map((notice) => notice.text);
}

function reconnecting(attempt: number): string {
	return `Reconnecting to dalgona (attempt ${attempt} of 3).`;
}

function portAt(ports: readonly FakePort[], index: number): FakePort {
	const port = ports[index];
	if (port === undefined) throw new Error(`no port ${index}`);
	return port;
}

async function startConnected(session: Session): Promise<void> {
	session.start(location);
	await vi.advanceTimersByTimeAsync(0);
	expect(session.connection).toBe("connected");
}

describe("automatic reconnect", () => {
	beforeEach(() => {
		vi.useFakeTimers();
		vi.stubGlobal("history", { replaceState: vi.fn() });
	});

	afterEach(() => {
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});

	test("reconnects with backoff and stops after three", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial);
		await startConnected(session);
		portAt(ports, 0).drop();
		expect(session.connection).toBe("disconnected");
		expect(noticeTexts(session)).toEqual([reconnecting(1)]);

		await vi.advanceTimersByTimeAsync(999);
		expect(ports).toHaveLength(1);
		await vi.advanceTimersByTimeAsync(1);
		expect(ports).toHaveLength(2);
		portAt(ports, 1).drop();
		expect(noticeTexts(session)).toEqual([reconnecting(2)]);

		await vi.advanceTimersByTimeAsync(1999);
		expect(ports).toHaveLength(2);
		await vi.advanceTimersByTimeAsync(1);
		expect(ports).toHaveLength(3);
		portAt(ports, 2).drop();
		expect(noticeTexts(session)).toEqual([reconnecting(3)]);

		await vi.advanceTimersByTimeAsync(3999);
		expect(ports).toHaveLength(3);
		await vi.advanceTimersByTimeAsync(1);
		expect(ports).toHaveLength(4);
		portAt(ports, 3).drop();
		expect(noticeTexts(session)).toEqual(["Disconnected from dalgona."]);
		const notice = session.notices[0];
		expect(notice?.tone).toBe("error");
		expect(notice?.actions.map((action) => action.label)).toEqual([
			"Reconnect",
		]);

		await vi.advanceTimersByTimeAsync(60000);
		expect(ports).toHaveLength(4);
		session.stop();
	});

	test("a failed attempt counts and adds no error notice", async () => {
		const { dial, ports } = fakeDial("none", {}, (index) =>
			index === 0 ? "open" : "refuse",
		);
		const session = new Session(dial);
		await startConnected(session);
		portAt(ports, 0).drop();

		await vi.advanceTimersByTimeAsync(1000);
		expect(ports).toHaveLength(2);
		expect(noticeTexts(session)).toEqual([reconnecting(2)]);
		await vi.advanceTimersByTimeAsync(2000);
		expect(ports).toHaveLength(3);
		expect(noticeTexts(session)).toEqual([reconnecting(3)]);
		await vi.advanceTimersByTimeAsync(4000);
		expect(ports).toHaveLength(4);
		expect(noticeTexts(session)).toEqual(["Disconnected from dalgona."]);
		await vi.advanceTimersByTimeAsync(60000);
		expect(ports).toHaveLength(4);
		session.stop();
	});

	test("an attempt that never opens fails at the 30 second deadline", async () => {
		const { dial, ports } = fakeDial("none", {}, (index) =>
			index === 1 ? "hang" : "open",
		);
		const session = new Session(dial);
		await startConnected(session);
		portAt(ports, 0).drop();

		await vi.advanceTimersByTimeAsync(1000);
		expect(ports).toHaveLength(2);
		await vi.advanceTimersByTimeAsync(29999);
		expect(noticeTexts(session)).toEqual([reconnecting(1)]);
		await vi.advanceTimersByTimeAsync(1);
		expect(noticeTexts(session)).toEqual([reconnecting(2)]);
		await vi.advanceTimersByTimeAsync(1999);
		expect(ports).toHaveLength(2);
		await vi.advanceTimersByTimeAsync(1);
		expect(ports).toHaveLength(3);
		await vi.advanceTimersByTimeAsync(0);
		expect(session.connection).toBe("connected");
		expect(noticeTexts(session)).toEqual([]);
		session.stop();
	});

	test("resets the budget after 20 seconds", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial);
		await startConnected(session);
		portAt(ports, 0).drop();
		await vi.advanceTimersByTimeAsync(1000);
		expect(ports).toHaveLength(2);
		expect(session.connection).toBe("connected");
		expect(noticeTexts(session)).toEqual([]);

		await vi.advanceTimersByTimeAsync(20000);
		portAt(ports, 1).drop();
		expect(noticeTexts(session)).toEqual([reconnecting(1)]);
		session.stop();
	});

	test("keeps the budget when the connection drops within 20 seconds", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial);
		await startConnected(session);
		portAt(ports, 0).drop();
		await vi.advanceTimersByTimeAsync(1000);
		expect(ports).toHaveLength(2);

		await vi.advanceTimersByTimeAsync(19999);
		portAt(ports, 1).drop();
		expect(noticeTexts(session)).toEqual([reconnecting(2)]);

		await vi.advanceTimersByTimeAsync(2000);
		expect(ports).toHaveLength(3);
		portAt(ports, 2).drop();
		expect(noticeTexts(session)).toEqual([reconnecting(3)]);
		session.stop();
	});

	test("a manual reconnect cancels the schedule", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial);
		await startConnected(session);
		portAt(ports, 0).drop();
		const notice = session.notices.find(
			(entry) => entry.text === reconnecting(1),
		);
		const action = notice?.actions.find((entry) => entry.label === "Reconnect");
		expect(action).toBeDefined();

		await vi.advanceTimersByTimeAsync(500);
		action?.run();
		expect(ports).toHaveLength(2);
		await vi.advanceTimersByTimeAsync(0);
		expect(session.connection).toBe("connected");
		expect(noticeTexts(session)).toEqual([]);

		await vi.advanceTimersByTimeAsync(10000);
		expect(ports).toHaveLength(2);
		session.stop();
	});

	test("the first connect of a page load does not retry", async () => {
		const { dial, ports } = fakeDial("none", {}, () => "refuse");
		const session = new Session(dial);
		session.start(location);
		await vi.advanceTimersByTimeAsync(60000);
		expect(ports).toHaveLength(1);
		expect(session.connection).toBe("disconnected");
		expect(noticeTexts(session)).toEqual([
			"Connection error: websocket error.",
			"Unable to connect to dalgona: connection closed. Check that dalgona web is still running.",
		]);
		session.stop();
	});

	test("a reconnect resumes the open thread, keeps queued follow-ups and starts no turn", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		await startConnected(session);
		await session.selectThread("t1");
		expect(session.running).toBe(true);
		session.queueFollowup("later");
		portAt(ports, 0).drop();

		await vi.advanceTimersByTimeAsync(1000);
		const second = portAt(ports, 1);
		expect(second.calls).toContain("thread/resume");
		expect(second.calls).not.toContain("turn/start");
		expect(second.calls).not.toContain("turn/steer");
		expect(session.activeThreadId).toBe("t1");
		expect(session.loadingTranscript).toBe(false);
		expect(session.queuedFollowups).toEqual(["later"]);
		expect(noticeTexts(session)).toEqual([]);
		session.stop();
	});
});

function sentText(params: JsonValue): string {
	if (typeof params !== "object" || params === null || !("input" in params)) {
		return "";
	}
	const { input } = params;
	const first = Array.isArray(input) ? input[0] : undefined;
	if (typeof first !== "object" || first === null || !("text" in first)) {
		return "";
	}
	const { text } = first;
	return typeof text === "string" ? text : "";
}

function failingOnce(sent: string[]): Answer {
	let failed = false;
	return (params) => {
		if (!failed) {
			failed = true;
			throw new Error("network down");
		}
		sent.push(sentText(params));
		return { turn: { id: "turn-2", status: "inProgress", items: [] } };
	};
}

function completion(threadId: string, turnId: string): AppNotification {
	const parsed = parseNotification({
		method: "turn/completed",
		params: {
			threadId,
			turn: { id: turnId, status: "completed", items: [] },
		},
	});
	if (parsed === null) throw new Error("completion did not parse");
	return parsed;
}

function retryAction(session: Session, label: string): () => void {
	const notice = session.notices.find((entry) =>
		entry.text.startsWith("Unable to send message"),
	);
	const action = notice?.actions.find((entry) => entry.label === label);
	if (action === undefined) throw new Error(`no ${label} action`);
	return action.run;
}

describe("failed follow-up sends", () => {
	beforeEach(() => {
		vi.stubGlobal("history", { replaceState: vi.fn() });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	test("a follow-up queued while idle is sent once when Try again succeeds", async () => {
		const sent: string[] = [];
		const { dial, ports } = fakeDial("none", {
			"turn/start": failingOnce(sent),
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");

		session.queueFollowup("later");
		await vi.waitFor(() => {
			expect(retryAction(session, "Try again")).toBeDefined();
		});
		expect(session.queuedFollowups).toEqual([]);

		retryAction(session, "Try again")();
		await vi.waitFor(() => {
			expect(sent).toEqual(["later"]);
		});
		ports[0]?.deliver(completion("t1", "turn-2"));
		await vi.waitFor(() => {
			expect(session.running).toBe(false);
		});
		expect(sent).toEqual(["later"]);
		session.stop();
	});

	test("a queued follow-up whose send fails is sent once when Try again succeeds", async () => {
		const sent: string[] = [];
		const { dial, ports } = fakeDial("t1", {
			"turn/start": failingOnce(sent),
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");

		session.queueFollowup("next");
		expect(session.queuedFollowups).toEqual(["next"]);
		ports[0]?.deliver(completion("t1", "turn-1"));
		await vi.waitFor(() => {
			expect(retryAction(session, "Try again")).toBeDefined();
		});

		retryAction(session, "Try again")();
		await vi.waitFor(() => {
			expect(sent).toEqual(["next"]);
		});
		ports[0]?.deliver(completion("t1", "turn-2"));
		await vi.waitFor(() => {
			expect(session.running).toBe(false);
		});
		expect(sent).toEqual(["next"]);
		session.stop();
	});
});

function turnNotice(
	method: "turn/started" | "turn/completed",
	threadId: string,
	status: string,
): AppNotification {
	const parsed = parseNotification({
		method,
		params: {
			threadId,
			turn: { id: "turn-2", status, items: [], error: null },
		},
	});
	if (parsed === null) throw new Error("turn notice did not parse");
	return parsed;
}

describe("completed turns", () => {
	beforeEach(() => {
		vi.stubGlobal("history", { replaceState: vi.fn() });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	test("marks a thread when its turn completes and clears the mark when the next turn starts", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial);
		session.start(location);
		await vi.waitFor(() => {
			expect(session.connection).toBe("connected");
		});
		const port = ports[0];
		if (port === undefined) throw new Error("no port was dialled");
		port.deliver(turnNotice("turn/completed", "t2", "completed"));
		expect(session.completedThreadIds.has("t2")).toBe(true);
		port.deliver(turnNotice("turn/started", "t2", "inProgress"));
		expect(session.completedThreadIds.has("t2")).toBe(false);
		session.stop();
	});

	test("clears the mark when the last turn ends in another state", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial);
		session.start(location);
		await vi.waitFor(() => {
			expect(session.connection).toBe("connected");
		});
		const port = ports[0];
		if (port === undefined) throw new Error("no port was dialled");
		port.deliver(turnNotice("turn/completed", "t2", "completed"));
		port.deliver(turnNotice("turn/completed", "t2", "interrupted"));
		expect(session.completedThreadIds.has("t2")).toBe(false);
		session.stop();
	});

	test("reads the mark from the last turn of a loaded thread", async () => {
		const completedResume: Answer = (params) => ({
			thread: thread(threadIdOf(params), [
				{ id: "turn-1", status: "completed", items: [] },
			]),
			model: "gpt-test",
			approvalsReviewer: "user",
			modelProvider: "openai",
			approvalPolicy: "on-request",
			sandbox: "workspace-write",
			cwd: "/work",
		});
		const { dial } = fakeDial("none", { "thread/resume": completedResume });
		const session = new Session(dial);
		session.start(location);
		await vi.waitFor(() => {
			expect(session.connection).toBe("connected");
		});
		await session.selectThread("t2");
		expect(session.completedThreadIds.has("t2")).toBe(true);
		session.stop();
	});

	test("keeps the token usage a resumed thread reports", async () => {
		const breakdown = {
			totalTokens: 27,
			inputTokens: 20,
			cachedInputTokens: 0,
			cacheWriteInputTokens: 0,
			outputTokens: 7,
			reasoningOutputTokens: 0,
		};
		const resumeWithUsage: Answer = (params) => ({
			thread: thread(threadIdOf(params), []),
			model: "gpt-test",
			approvalsReviewer: "user",
			modelProvider: "openai",
			approvalPolicy: "on-request",
			sandbox: "workspace-write",
			cwd: "/work",
			tokenUsage: {
				last: breakdown,
				total: breakdown,
				modelContextWindow: 200000,
			},
		});
		const { dial } = fakeDial("none", { "thread/resume": resumeWithUsage });
		const session = new Session(dial);
		session.start(location);
		await vi.waitFor(() => {
			expect(session.connection).toBe("connected");
		});
		await session.selectThread("t2");
		expect(session.usage).toHaveProperty(["t2", "contextWindow"], 200000);
		expect(session.usage).toHaveProperty(["t2", "total", "totalTokens"], 27);
		session.stop();
	});
});

describe("file search", () => {
	beforeEach(() => {
		vi.stubGlobal("history", { replaceState: vi.fn() });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	test("searches under the open thread's cwd when the thread is missing from the list", async () => {
		const searches: JsonValue[] = [];
		const { dial, ports } = fakeDial("none", {
			"thread/resume": () => ({
				thread: {
					id: "t1",
					updatedAt: 1,
					status: { type: "idle" },
					turns: [],
				},
				model: "gpt-test",
				approvalsReviewer: "user",
				modelProvider: "openai",
				approvalPolicy: "on-request",
				sandbox: "workspace-write",
				cwd: "/work",
			}),
			fuzzyFileSearch: (params) => {
				searches.push(params);
				return { files: [] };
			},
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		session.threads = [];

		await session.searchFiles("mod");

		expect(searches).toEqual([
			{ query: "mod", roots: ["/work"], cancellationToken: "composer" },
		]);
		expect(ports[0]?.calls).toContain("fuzzyFileSearch");
		session.stop();
	});

	test("searches under a newly started thread's response cwd", async () => {
		const searches: JsonValue[] = [];
		const { dial } = fakeDial("none", {
			"thread/start": () => ({
				thread: thread("new-thread", []),
				model: "gpt-test",
				approvalsReviewer: "user",
				modelProvider: "openai",
				approvalPolicy: "on-request",
				sandbox: "workspace-write",
				cwd: "/new-work",
			}),
			fuzzyFileSearch: (params) => {
				searches.push(params);
				return { files: [] };
			},
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		await session.createThread();
		session.threads = [];

		await session.searchFiles("mod");

		expect(searches).toEqual([
			{ query: "mod", roots: ["/new-work"], cancellationToken: "composer" },
		]);
		session.stop();
	});

	test("rejects a null response cwd", () => {
		const response = resumeAnswer("none")({ threadId: "t1" });
		expect(() =>
			parseThreadStartResponse({ ...(response as object), cwd: null }),
		).toThrow("invalid thread/start");
	});

	test("file search uses the cwd of the thread/start response", async () => {
		const searches: JsonValue[] = [];
		const { dial } = fakeDial("none", {
			"thread/start": () => ({
				thread: { ...thread("new-thread", []), cwd: "/other" },
				model: "gpt-test",
				approvalsReviewer: "user",
				modelProvider: "openai",
				approvalPolicy: "on-request",
				sandbox: "workspace-write",
				cwd: "/repo",
			}),
			fuzzyFileSearch: (params) => {
				searches.push(params);
				return { files: [] };
			},
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		await session.createThread();
		session.threads = [];

		await session.searchFiles("mod");

		expect(searches).toEqual([
			{ query: "mod", roots: ["/repo"], cancellationToken: "composer" },
		]);
		session.stop();
	});

	test("a thread/start response without cwd is an InvalidPayload", () => {
		const response = resumeAnswer("none")({ threadId: "t1" }) as {
			readonly [key: string]: JsonValue;
		};
		const { cwd: _cwd, ...withoutCwd } = response;
		let caught: unknown;
		try {
			parseThreadStartResponse(withoutCwd);
		} catch (error) {
			caught = error;
		}
		expect(caught).toBeInstanceOf(Error);
		expect((caught as Error).name).toBe("InvalidPayload");
		expect((caught as Error).message).toBe("invalid thread/start");
	});

	test("sends no search when no thread is open", async () => {
		const { dial, ports } = fakeDial("none", {
			"thread/list": () => ({ data: [], nextCursor: null }),
		});
		const session = new Session(dial);
		session.start(location);
		await vi.waitFor(() => {
			expect(session.connection).toBe("connected");
		});

		expect(await session.searchFiles("mod")).toEqual([]);
		expect(ports[0]?.calls).not.toContain("fuzzyFileSearch");
		session.stop();
	});
});

function holdMethod(port: FakePort, method: string, gate: Promise<void>): void {
	const call = port.call.bind(port);
	port.call = <T>(
		name: string,
		params: JsonValue,
		parse: (value: unknown) => T,
	): Promise<T> =>
		name === method
			? gate.then(() => call(name, params, parse))
			: call(name, params, parse);
}

function turnStarted(threadId: string, turnId: string): AppNotification {
	const parsed = parseNotification({
		method: "turn/started",
		params: {
			threadId,
			turn: { id: turnId, status: "inProgress", items: [], error: null },
		},
	});
	if (parsed === null) throw new Error("turn start did not parse");
	return parsed;
}

function fileChangeCompleted(
	threadId: string,
	turnId: string,
	itemId: string,
): AppNotification {
	const parsed = parseNotification({
		method: "item/completed",
		params: {
			completedAtMs: 1,
			item: {
				id: itemId,
				type: "fileChange",
				status: "completed",
				changes: [],
			},
			threadId,
			turnId,
		},
	});
	if (parsed === null) throw new Error("file change did not parse");
	return parsed;
}

function toolJudged(verdict: string, summary: string): AppNotification {
	const parsed = parseNotification({
		method: "dalgona/tool/judged",
		params: {
			threadId: "t1",
			turnId: "turn-1",
			itemId: "call-7",
			verdict,
			summary,
			paths: ["a.go"],
		},
	});
	if (parsed === null) throw new Error("tool judged did not parse");
	return parsed;
}

function turnJudged(verdict: string, summary: string): AppNotification {
	const parsed = parseNotification({
		method: "dalgona/turn/judged",
		params: {
			threadId: "t1",
			turnId: "turn-1",
			verdict,
			summary,
			paths: [],
		},
	});
	if (parsed === null) throw new Error("turn judged did not parse");
	return parsed;
}

describe("review note", () => {
	beforeEach(() => {
		vi.stubGlobal("history", { replaceState: vi.fn() });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	async function openedSession(): Promise<{
		session: Session;
		port: FakePort;
	}> {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		const port = ports[0];
		if (port === undefined) throw new Error("no port was dialled");
		return { session, port };
	}

	test("a flagged tool verdict sets the review note", async () => {
		const { session, port } = await openedSession();
		port.deliver(turnStarted("t1", "turn-1"));
		port.deliver(fileChangeCompleted("t1", "turn-1", "call-7"));
		port.deliver(toolJudged("flagged", "Flagged: removed code"));
		expect(session.reviewNote).toBe("Flagged: removed code");
		session.stop();
	});

	test("a clean tool verdict sets no review note", async () => {
		const { session, port } = await openedSession();
		port.deliver(turnStarted("t1", "turn-1"));
		port.deliver(fileChangeCompleted("t1", "turn-1", "call-7"));
		port.deliver(toolJudged("clean", "No problems found"));
		expect(session.reviewNote).toBeNull();
		session.stop();
	});

	test("an error turn verdict sets the review note", async () => {
		const { session, port } = await openedSession();
		port.deliver(turnStarted("t1", "turn-1"));
		port.deliver(fileChangeCompleted("t1", "turn-1", "call-7"));
		port.deliver(turnJudged("error", "Failed: judge unreachable"));
		expect(session.reviewNote).toBe("Failed: judge unreachable");
		session.stop();
	});

	test("a turnStatus change clears the review note", async () => {
		const { session, port } = await openedSession();
		port.deliver(turnStarted("t1", "turn-1"));
		port.deliver(fileChangeCompleted("t1", "turn-1", "call-7"));
		port.deliver(toolJudged("flagged", "Flagged: removed code"));
		expect(session.reviewNote).toBe("Flagged: removed code");

		port.deliver(completion("t1", "turn-1"));

		expect(session.reviewNote).toBeNull();
		session.stop();
	});
});

describe("recall history", () => {
	beforeEach(() => {
		vi.stubGlobal("history", { replaceState: vi.fn() });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	test("an image-only message adds no empty entry to the recall history", async () => {
		const { dial } = fakeDial("none", {
			"turn/start": () => ({
				turn: { id: "turn-2", status: "inProgress", items: [] },
			}),
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		await session.sendMessage("first");
		await session.sendMessage("", {
			texts: [],
			images: [
				{
					kind: "image",
					id: 1,
					name: "shot.png",
					mime: "image/png",
					size: 8,
					dataUrl: "data:image/png;base64,iVBORw0KGgo=",
				},
			],
		});
		expect(session.history).toEqual(["first"]);
		session.stop();
	});
});

describe("shell mode", () => {
	beforeEach(() => {
		vi.stubGlobal("history", { replaceState: vi.fn() });
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	function withCwd(): Answer {
		return () => ({
			data: [
				{
					id: "t1",
					updatedAt: 1,
					status: { type: "idle" },
					turns: [],
					cwd: "/work",
				},
			],
			nextCursor: null,
		});
	}

	function withCwdOnBoth(): Answer {
		return () => ({
			data: [
				{
					id: "t1",
					updatedAt: 1,
					status: { type: "idle" },
					turns: [],
					cwd: "/work",
				},
				{
					id: "t2",
					updatedAt: 2,
					status: { type: "idle" },
					turns: [],
					cwd: "/work",
				},
			],
			nextCursor: null,
		});
	}

	test("shell mode calls command/exec with the platform argv and shows the result", async () => {
		const sent: JsonValue[] = [];
		const { dial, ports } = fakeDial("none", {
			"thread/list": withCwd(),
			"command/exec": (params) => {
				sent.push(params);
				return { exitCode: 0, stdout: "hi\n", stderr: "" };
			},
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");

		const run = await session.runShell("echo hi");

		expect(sent).toEqual([
			{
				command: ["sh", "-c", "echo hi"],
				cwd: "/work",
				timeoutMs: 120000,
				outputBytesCap: 65536,
			},
		]);
		expect(run).toEqual({
			n: 1,
			exitCode: 0,
			lines: 1,
			text: "Output of `echo hi` (exit 0):\n```\nhi\n```",
		});
		expect(ports[0]?.calls).not.toContain("turn/start");
		expect(session.transcript.items.at(-1)).toMatchObject({
			kind: "commandExecution",
			command: "echo hi",
			output: "hi",
			status: "completed",
			exitCode: 0,
		});
		session.stop();
	});

	test("shell mode runs cmd on windows", async () => {
		const sent: JsonValue[] = [];
		const { dial } = fakeDial("none", {
			initialize: () => ({
				userAgent: "dalgona/0.1.0",
				platformFamily: "windows",
				platformOs: "windows",
			}),
			"thread/list": withCwd(),
			"command/exec": (params) => {
				sent.push(params);
				return { exitCode: 0, stdout: "", stderr: "" };
			},
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");

		await session.runShell("dir");

		expect(sent[0]).toMatchObject({ command: ["cmd", "/C", "dir"] });
		session.stop();
	});

	test("a command that finishes after another thread opened shows no row or chip there", async () => {
		const { dial, ports } = fakeDial("none", {
			"thread/list": withCwdOnBoth(),
			"command/exec": () => ({ exitCode: 0, stdout: "late\n", stderr: "" }),
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");
		const port = ports[0];
		if (port === undefined) throw new Error("no port");
		let release = (): void => {};
		const gate = new Promise<void>((resolve) => {
			release = resolve;
		});
		holdMethod(port, "command/exec", gate);

		const run = session.runShell("ls");
		if (run === null) throw new Error("the command did not start");
		await session.selectThread("t2");
		release();

		expect(await run).toBeNull();
		expect(session.transcript.threadId).toBe("t2");
		expect(
			session.transcript.items.some((item) => item.kind === "commandExecution"),
		).toBe(false);
		session.stop();
	});

	test("a shell command with no open thread shows a notice, runs nothing and keeps no chip", () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial);

		expect(session.runShell("ls")).toBeNull();
		expect(session.notices.map((notice) => notice.text)).toContain(
			"Open a thread before running a shell command",
		);
		expect(ports).toHaveLength(0);
	});

	test("a shell command in a thread with no working directory shows the same notice", async () => {
		const bare = {
			id: "t1",
			updatedAt: 1,
			status: { type: "idle" },
			turns: [],
		};
		const { dial, ports } = fakeDial("none", {
			"thread/list": () => ({ data: [bare], nextCursor: null }),
			"thread/read": () => ({ thread: bare }),
			"thread/resume": () => ({
				thread: bare,
				model: "gpt-test",
				approvalsReviewer: "user",
				modelProvider: "openai",
				approvalPolicy: "on-request",
				sandbox: "workspace-write",
				cwd: "",
			}),
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");

		expect(session.runShell("ls")).toBeNull();
		expect(session.notices.map((notice) => notice.text)).toContain(
			"Open a thread before running a shell command",
		);
		expect(ports[0]?.calls).not.toContain("command/exec");
		session.stop();
	});

	test("a shell command whose call fails shows a failed row and makes no chip", async () => {
		const { dial } = fakeDial("none", {
			"thread/list": withCwd(),
			"command/exec": () => {
				throw new Error("sandbox refused");
			},
		});
		const session = new Session(dial);
		session.start(location);
		await openThread(session, "t1");

		expect(await session.runShell("rm x")).toBeNull();
		expect(session.transcript.items.at(-1)).toMatchObject({
			kind: "commandExecution",
			command: "rm x",
			output: "sandbox refused",
			status: "failed",
			exitCode: null,
		});
		session.stop();
	});
});

describe("desktop notifications", () => {
	let permission: string;
	let visibility: string;
	let shown: { text: string; body: string; tag: string }[];
	let requestPermission: Mock;

	beforeEach(() => {
		vi.stubGlobal("history", { replaceState: vi.fn() });
		permission = "granted";
		visibility = "hidden";
		shown = [];
		requestPermission = vi.fn(() => {
			permission = "granted";
			return Promise.resolve("granted");
		});
		class FakeNotification {
			static get permission(): string {
				return permission;
			}
			static requestPermission = requestPermission;
			constructor(text: string, options: { body: string; tag: string }) {
				shown.push({ text, body: options.body, tag: options.tag });
			}
		}
		vi.stubGlobal("Notification", FakeNotification);
		vi.stubGlobal("document", {
			get visibilityState(): string {
				return visibility;
			},
		});
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	test("notifies on a waiting approval only when hidden and granted", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial, () => {});
		session.start(location);
		await vi.waitFor(() => {
			expect(session.connection).toBe("connected");
		});
		await vi.waitFor(() => {
			expect(session.threads).toHaveLength(2);
		});
		const port = ports[0];
		if (port === undefined) throw new Error("no port was dialled");

		visibility = "visible";
		port.request(approval("req-1"));
		expect(shown).toEqual([]);

		visibility = "hidden";
		permission = "default";
		port.request(approval("req-2"));
		expect(shown).toEqual([]);

		permission = "granted";
		port.request(approval("req-3"));
		expect(shown).toEqual([
			{ text: "Waiting for your reply", body: "Untitled thread", tag: "t1" },
		]);
		session.stop();
	});

	test("notifies on a waiting question only when hidden and granted", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial, () => {});
		session.start(location);
		await vi.waitFor(() => {
			expect(session.threads).toHaveLength(2);
		});
		const port = ports[0];
		if (port === undefined) throw new Error("no port was dialled");

		visibility = "visible";
		port.question(question("req-1"));
		expect(shown).toEqual([]);

		visibility = "hidden";
		permission = "default";
		port.question(question("req-2"));
		expect(shown).toEqual([]);

		permission = "granted";
		port.question(question("req-3"));
		expect(shown).toEqual([
			{ text: "Waiting for your reply", body: "Untitled thread", tag: "t1" },
		]);
		session.stop();
	});

	test("never asks for permission without a click", async () => {
		permission = "default";
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial, () => {});
		session.start(location);
		await vi.waitFor(() => {
			expect(session.connection).toBe("connected");
		});
		const port = ports[0];
		if (port === undefined) throw new Error("no port was dialled");

		port.request(approval("req-1"));
		port.deliver(turnNotice("turn/completed", "t1", "completed"));
		expect(requestPermission).not.toHaveBeenCalled();
		expect(session.desktopNotifications).toBe("off");

		await session.requestDesktopNotifications();
		expect(requestPermission).toHaveBeenCalledTimes(1);
		expect(session.desktopNotifications).toBe("on");
		session.stop();
	});

	test("an interrupted turn calls notifyAttention", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial, () => {});
		session.start(location);
		await vi.waitFor(() => {
			expect(session.connection).toBe("connected");
		});
		const port = ports[0];
		if (port === undefined) throw new Error("no port was dialled");

		const attention = vi.spyOn(session, "notifyAttention");
		port.deliver(turnNotice("turn/completed", "t2", "interrupted"));
		expect(attention).toHaveBeenCalledWith("interrupted", "t2");
		session.stop();
	});

	test("a throwing Notification constructor does not skip the queued follow-up", async () => {
		vi.stubGlobal(
			"Notification",
			class ThrowingNotification {
				static get permission(): string {
					return "granted";
				}
				static requestPermission = requestPermission;
				constructor() {
					throw new Error("illegal constructor");
				}
			},
		);
		const sent: string[] = [];
		const { dial, ports } = fakeDial("t1", {
			"turn/start": (params) => {
				sent.push(sentText(params));
				return { turn: { id: "turn-2", status: "inProgress", items: [] } };
			},
		});
		const session = new Session(dial, () => {});
		session.start(location);
		await openThread(session, "t1");

		session.queueFollowup("next");
		ports[0]?.deliver(completion("t1", "turn-1"));
		await vi.waitFor(() => {
			expect(sent).toEqual(["next"]);
		});
		session.stop();
	});

	test("names a finished turn by its state word", async () => {
		const { dial, ports } = fakeDial("none");
		const session = new Session(dial, () => {});
		session.start(location);
		await vi.waitFor(() => {
			expect(session.connection).toBe("connected");
		});
		const port = ports[0];
		if (port === undefined) throw new Error("no port was dialled");

		port.deliver(turnNotice("turn/completed", "t2", "completed"));
		port.deliver(turnNotice("turn/completed", "t2", "failed"));
		port.deliver(turnNotice("turn/completed", "t2", "interrupted"));
		expect(shown.map((entry) => entry.text)).toEqual([
			"Done",
			"Failed",
			"Stopped",
		]);
		session.stop();
	});
});

function approval(id: string): ApprovalRequest {
	return {
		id,
		method: "item/commandExecution/requestApproval",
		threadId: "t1",
		turnId: "turn-1",
		itemId: "call-1",
		title: "Run this command?",
		detail: "ls",
		reason: null,
	};
}

function question(id: string): QuestionRequest {
	return {
		id,
		method: QUESTION_METHOD,
		threadId: "t1",
		turnId: "turn-1",
		itemId: "call-1",
		questions: [
			{
				id: "q1",
				header: "Database",
				question: "Which database should the service use?",
				options: [
					{
						label: "Postgres",
						description: "Managed Postgres.",
						preview: null,
					},
				],
				multiSelect: false,
				isOther: false,
			},
		],
	};
}
