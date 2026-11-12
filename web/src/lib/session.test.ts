import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { type RpcPort, Session } from "./session.svelte";
import type {
	AppNotification,
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
		initialize: () => ({ userAgent: "dalgona/0.1.0", platformOs: "linux" }),
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
		session.changeModel("gpt-next");
		await vi.waitFor(() => {
			expect(session.notices.map((notice) => notice.text)).toContain(
				"Unable to save model selection: no answer for config/value/write.",
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
			ports[0]?.calls.filter((method) => method === "config/value/write"),
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
