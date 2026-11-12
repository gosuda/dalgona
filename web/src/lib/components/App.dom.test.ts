import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import App from "#lib/components/App.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type {
	ApprovalRequest,
	JsonValue,
	RpcHandlers,
	ThreadActivity,
	ThreadSummary,
} from "#lib/types.ts";
import { parseNotification } from "#lib/validation.ts";

function idlePort(): RpcPort {
	return {
		call: () => Promise.reject(new Error("no call in this test")),
		notify: (): void => undefined,
		respond: (): void => undefined,
		open: () => Promise.resolve(),
		close: (): void => undefined,
	};
}

function sessionWithRequestElsewhere(): Session {
	const dial = (_url: string, _handlers: RpcHandlers): RpcPort => idlePort();
	const session = new Session(dial);
	const request: ApprovalRequest = {
		id: "req-1",
		method: "item/commandExecution/requestApproval",
		threadId: "thread-other",
		turnId: "turn-1",
		itemId: "item-1",
		title: "Run command",
		detail: "ls",
		reason: null,
	};
	session.requests = [request];
	return session;
}

describe("requests elsewhere", () => {
	afterEach(() => {
		cleanup();
	});

	test("shows the waiting note inside main and opens that thread", async () => {
		const session = sessionWithRequestElsewhere();
		const select = vi
			.spyOn(session, "selectThread")
			.mockResolvedValue(undefined);
		render(App, { props: { session } });
		const note = screen.getByText("1 request waiting on another thread.");
		expect(note.closest("main")).not.toBeNull();
		const user = userEvent.setup();
		await user.click(screen.getByRole("button", { name: "Open thread" }));
		expect(select).toHaveBeenCalledWith("thread-other");
	});
});

function sessionWithHandler(): { session: Session; handlers: RpcHandlers } {
	let handlers: RpcHandlers | undefined;
	const session = new Session((_url, incoming) => {
		handlers = incoming;
		return idlePort();
	});
	session.start({
		hash: "#token=abc",
		host: "localhost:7437",
		pathname: "/",
		protocol: "http:",
		search: "",
	});
	if (handlers === undefined) throw new Error("the session did not dial");
	return { session, handlers };
}

function parsed(method: string, params: JsonValue) {
	const notification = parseNotification({ method, params });
	if (notification === null) throw new Error(`${method} did not parse`);
	return notification;
}

function threadSummary(
	status: ThreadActivity,
	activeFlags: readonly string[],
): ThreadSummary {
	return {
		id: "thread-1",
		title: "hello",
		preview: "hello",
		status,
		activeFlags: [...activeFlags],
		updatedAt: 1,
		model: null,
		cwd: "/work/app",
	};
}

describe("elapsed counter under reduced motion", () => {
	afterEach(() => {
		cleanup();
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});

	test("the counter moves as the transcript changes", async () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date(1_700_000_000_000));
		vi.stubGlobal("matchMedia", (query: string) => ({
			matches: query.includes("reduce"),
			media: query,
			addEventListener: () => {},
			removeEventListener: () => {},
		}));
		const { session, handlers } = sessionWithHandler();
		session.activeThreadId = "thread-1";
		handlers.onNotification(
			parsed("thread/started", {
				thread: {
					id: "thread-1",
					updatedAt: 1,
					status: { type: "idle" },
					turns: [],
				},
			}),
		);
		handlers.onNotification(
			parsed("turn/started", {
				threadId: "thread-1",
				turn: {
					id: "turn-2",
					status: "inProgress",
					items: [],
					startedAt: 1_700_000_000,
				},
			}),
		);
		const { container } = render(App, { props: { session } });
		await tick();
		expect(container.querySelector(".thread-head")?.textContent).toContain(
			"0 ms",
		);

		vi.setSystemTime(new Date(1_700_000_003_000));
		handlers.onNotification(
			parsed("item/agentMessage/delta", {
				threadId: "thread-1",
				turnId: "turn-2",
				itemId: "message-1",
				delta: "working",
			}),
		);
		await tick();
		expect(container.querySelector(".thread-head")?.textContent).toContain(
			"3s",
		);
		session.stop();
	});
});

describe("thread header state", () => {
	afterEach(() => {
		cleanup();
	});

	function openTurn(flags: readonly string[]) {
		const { session, handlers } = sessionWithHandler();
		session.activeThreadId = "thread-1";
		handlers.onNotification(
			parsed("thread/started", {
				thread: {
					id: "thread-1",
					name: "hello",
					updatedAt: 1,
					status: { type: "active", activeFlags: [...flags] },
					turns: [],
				},
			}),
		);
		handlers.onNotification(
			parsed("turn/started", {
				threadId: "thread-1",
				turn: { id: "turn-2", status: "inProgress", items: [] },
			}),
		);
		return session;
	}

	test("shows Waiting while the open thread waits on an approval", () => {
		const { container } = render(App, {
			props: { session: openTurn(["waitingOnApproval"]) },
		});
		expect(container.querySelector(".thread-head")?.textContent).toContain(
			"Waiting",
		);
	});

	test("shows Waiting while the open thread waits on a user answer", () => {
		const { container } = render(App, {
			props: { session: openTurn(["waitingOnUserInput"]) },
		});
		expect(container.querySelector(".thread-head")?.textContent).toContain(
			"Waiting",
		);
	});

	test("shows Working while the open thread runs with no waiting flag", () => {
		const { container } = render(App, { props: { session: openTurn([]) } });
		const header = container.querySelector(".thread-head")?.textContent ?? "";
		expect(header).toContain("Working");
		expect(header).not.toContain("Waiting");
	});

	test("the document title carries the state word", () => {
		const session = openTurn([]);
		render(App, { props: { session } });
		expect(document.title).toBe("dalgona · hello · Working");
	});

	test("the document title names Ready for an idle open thread", () => {
		const { session } = sessionWithHandler();
		session.threads = [threadSummary("idle", [])];
		session.activeThreadId = "thread-1";
		render(App, { props: { session } });
		expect(document.title).toBe("dalgona · hello · Ready");
	});

	test("the document title names Offline when the connection is down", () => {
		const { session } = sessionWithHandler();
		session.threads = [threadSummary("idle", [])];
		session.activeThreadId = "thread-1";
		session.connection = "disconnected";
		render(App, { props: { session } });
		expect(document.title).toBe("dalgona · hello · Offline");
	});
});

describe("file-change approval diff", () => {
	afterEach(() => {
		cleanup();
	});

	test("shows the diff of a live file-change approval before item/completed", async () => {
		const { session, handlers } = sessionWithHandler();
		session.activeThreadId = "thread-1";
		handlers.onNotification(
			parsed("item/fileChange/patchUpdated", {
				threadId: "thread-1",
				turnId: "turn-2",
				itemId: "fc-9",
				changes: [
					{
						path: "b.txt",
						kind: { type: "update" },
						diff: "--- a/b.txt\n+++ b/b.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n",
						additions: 1,
						deletions: 1,
					},
				],
			}),
		);
		handlers.onRequest({
			id: "req-9",
			method: "item/fileChange/requestApproval",
			threadId: "thread-1",
			turnId: "turn-2",
			itemId: "fc-9",
			title: "Apply file changes?",
			detail: "",
			reason: null,
		});
		render(App, { props: { session } });
		await tick();
		expect(screen.getByText("+gamma")).toBeTruthy();
		expect(screen.getByText("b.txt")).toBeTruthy();
		session.stop();
	});

	test("keeps the pending approval's diff when another thread sends the same item", async () => {
		const { session, handlers } = sessionWithHandler();
		session.activeThreadId = "thread-1";
		handlers.onNotification(
			parsed("item/fileChange/patchUpdated", {
				threadId: "thread-1",
				turnId: "turn-2",
				itemId: "fc-9",
				changes: [
					{
						path: "b.txt",
						kind: { type: "update" },
						diff: "--- a/b.txt\n+++ b/b.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n",
						additions: 1,
						deletions: 1,
					},
				],
			}),
		);
		handlers.onRequest({
			id: "req-9",
			method: "item/fileChange/requestApproval",
			threadId: "thread-1",
			turnId: "turn-2",
			itemId: "fc-9",
			title: "Apply file changes?",
			detail: "",
			reason: null,
		});
		handlers.onNotification(
			parsed("item/fileChange/patchUpdated", {
				threadId: "thread-2",
				turnId: "turn-9",
				itemId: "fc-9",
				changes: [
					{
						path: "c.txt",
						kind: { type: "update" },
						diff: "--- a/c.txt\n+++ b/c.txt\n@@ -1 +1 @@\n-one\n+delta\n",
						additions: 1,
						deletions: 1,
					},
				],
			}),
		);
		render(App, { props: { session } });
		await tick();
		expect(screen.getByText("+gamma")).toBeTruthy();
		expect(screen.queryByText("+delta")).toBeNull();
		session.stop();
	});
});
function commandApproval(id: string, itemId: string): ApprovalRequest {
	return {
		id,
		method: "item/commandExecution/requestApproval",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId,
		title: "Run command?",
		detail: "ls",
		reason: null,
	};
}

function fileApproval(id: string, itemId: string): ApprovalRequest {
	return {
		id,
		method: "item/fileChange/requestApproval",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId,
		title: "Apply file changes?",
		detail: "",
		reason: null,
	};
}

describe("request queue", () => {
	afterEach(() => {
		cleanup();
	});

	test("shows the oldest request and holds the next until it resolves", async () => {
		const { session, handlers } = sessionWithHandler();
		session.activeThreadId = "thread-1";
		handlers.onRequest(commandApproval("req-1", "cmd-1"));
		handlers.onRequest(fileApproval("req-2", "file-1"));
		render(App, { props: { session } });
		await tick();
		expect(
			screen.getByRole("heading", { name: "Run this command?" }),
		).toBeTruthy();
		expect(
			screen.queryByRole("heading", { name: "Apply these changes?" }),
		).toBeNull();
		expect(screen.getByText("Approval required · 1 of 2")).toBeTruthy();

		handlers.onNotification(
			parsed("serverRequest/resolved", {
				threadId: "thread-1",
				requestId: "req-1",
			}),
		);
		await tick();
		expect(
			screen.getByRole("heading", { name: "Apply these changes?" }),
		).toBeTruthy();
	});

	test("a sandbox choice waits behind an approval", async () => {
		const { session, handlers } = sessionWithHandler();
		session.activeThreadId = "thread-1";
		handlers.onRequest(fileApproval("req-1", "file-1"));
		handlers.onRequest(commandApproval("req-2", "cmd-1"));
		render(App, { props: { session } });
		await tick();
		expect(
			screen.getByRole("heading", { name: "Apply these changes?" }),
		).toBeTruthy();
		expect(
			screen.queryByRole("heading", { name: "Run this command?" }),
		).toBeNull();

		handlers.onNotification(
			parsed("serverRequest/resolved", {
				threadId: "thread-1",
				requestId: "req-1",
			}),
		);
		await tick();
		expect(
			screen.getByRole("heading", { name: "Run this command?" }),
		).toBeTruthy();
	});
});
