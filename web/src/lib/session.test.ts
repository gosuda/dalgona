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
}

function fakeDial(runningThread: string): {
	dial: (url: string, handlers: RpcHandlers) => RpcPort;
	ports: FakePort[];
} {
	const ports: FakePort[] = [];
	const dial = (_url: string, handlers: RpcHandlers): RpcPort => {
		const port = new FakePort(handlers, answers(runningThread));
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
});
