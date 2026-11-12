import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { type RpcPort, Session } from "./session.svelte";
import type {
	AppNotification,
	ApprovalRequest,
	JsonValue,
	RequestId,
	RpcHandlers,
} from "./types";
import { parseNotification } from "./validation";

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

function thread(id: string, turns: JsonValue): JsonValue {
	return {
		id,
		updatedAt: 1,
		status: { type: "idle" },
		turns,
		cwd: "/work",
	};
}

function resumeAnswer(runningThread: string): Answer {
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

class FakePort implements RpcPort {
	private readonly log: string[] = [];
	private readonly handlers: RpcHandlers;
	private readonly table: Record<string, Answer>;

	constructor(handlers: RpcHandlers, table: Record<string, Answer>) {
		this.handlers = handlers;
		this.table = table;
	}

	get calls(): readonly string[] {
		return this.log;
	}

	open(): Promise<void> {
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
	respond(_id: RequestId, _result: JsonValue): void {}

	close(): void {}

	drop(): void {
		this.handlers.onStateChange("disconnected");
	}
}

function fakeDial(
	runningThread: string,
	overrides: Record<string, Answer> = {},
): {
	dial: (url: string, handlers: RpcHandlers) => RpcPort;
	ports: FakePort[];
} {
	const ports: FakePort[] = [];
	const dial = (_url: string, handlers: RpcHandlers): RpcPort => {
		const port = new FakePort(handlers, {
			...answers(runningThread),
			...overrides,
		});
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

function resolvedNotice(threadId: string, requestId: string): AppNotification {
	const parsed = parseNotification({
		method: "serverRequest/resolved",
		params: { threadId, requestId },
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

	test("reconnect keeps queued follow-ups", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await waitForThread(session, "t1");
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
		await waitForThread(session, "t1");
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
		await waitForThread(session, "t1");
		for (const text of ["a", "b", "c"]) session.queueFollowup(text);
		expect(session.editFollowup(1)).toBe("b");
		expect(session.queuedFollowups).toEqual(["a", "c"]);
		session.stop();
	});

	test("removing a follow-up drops only that entry", async () => {
		const { dial } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await waitForThread(session, "t1");
		for (const text of ["a", "b", "c"]) session.queueFollowup(text);
		session.removeFollowup(1);
		expect(session.queuedFollowups).toEqual(["a", "c"]);
		session.stop();
	});

	test("opening another thread discards queued follow-ups", async () => {
		const { dial } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await waitForThread(session, "t1");
		session.queueFollowup("later");
		expect(session.queuedFollowups).toEqual(["later"]);

		await session.selectThread("t2");
		expect(session.activeThreadId).toBe("t2");
		expect(session.queuedFollowups).toEqual([]);
		session.stop();
	});

	test("forgets a file change's diff once its approval resolves", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await waitForThread(session, "t1");
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

	test("keeps a pending approval's diff across a thread switch", async () => {
		const { dial, ports } = fakeDial("t1");
		const session = new Session(dial);
		session.start(location);
		await waitForThread(session, "t1");
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
		await waitForThread(session, "t1");
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
		await waitForThread(session, "t1");
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
		await waitForThread(session, "t1");
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
		await waitForThread(session, "t1");
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
		await waitForThread(session, "t1");
		ports[0]?.drop();
		const notice = session.notices.find(
			(item) => item.text === "Disconnected from dalgona.",
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
		await waitForThread(session, "t1");

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
		await waitForThread(session, "t1");

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
			fuzzyFileSearch: (params) => {
				searches.push(params);
				return { files: [] };
			},
		});
		const session = new Session(dial);
		session.start(location);
		await waitForThread(session, "t1");
		session.threads = [];

		await session.searchFiles("mod");

		expect(searches).toEqual([
			{ query: "mod", roots: ["/work"], cancellationToken: "composer" },
		]);
		expect(ports[0]?.calls).toContain("fuzzyFileSearch");
		session.stop();
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

function holdCommand(port: FakePort, gate: Promise<void>): void {
	const call = port.call.bind(port);
	port.call = <T>(
		method: string,
		params: JsonValue,
		parse: (value: unknown) => T,
	): Promise<T> =>
		method === "command/exec"
			? gate.then(() => call(method, params, parse))
			: call(method, params, parse);
}

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
		await waitForThread(session, "t1");
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
		await waitForThread(session, "t1");

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
		await waitForThread(session, "t1");

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
		await waitForThread(session, "t1");
		const port = ports[0];
		if (port === undefined) throw new Error("no port");
		let release = (): void => {};
		const gate = new Promise<void>((resolve) => {
			release = resolve;
		});
		holdCommand(port, gate);

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
		await waitForThread(session, "t1");

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
		await waitForThread(session, "t1");

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
