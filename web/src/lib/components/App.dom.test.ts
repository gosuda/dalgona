import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import { slashCommands } from "#lib/commands.ts";
import App from "#lib/components/App.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type {
	ApprovalRequest,
	JsonValue,
	QuestionRequest,
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
function pendingPort(): RpcPort {
	return { ...idlePort(), open: () => new Promise<void>(() => undefined) };
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

function sessionWithHandler(port: () => RpcPort = idlePort): {
	session: Session;
	handlers: RpcHandlers;
} {
	let handlers: RpcHandlers | undefined;
	const session = new Session((_url, incoming) => {
		handlers = incoming;
		return port();
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

	test("keeps the turn start when the turn/start response follows the turn/started notification", async () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date(1_700_000_000_000));
		vi.stubGlobal("matchMedia", (query: string) => ({
			matches: query.includes("reduce"),
			media: query,
			addEventListener: () => {},
			removeEventListener: () => {},
		}));
		let responseTurn = "turn-2";
		let beforeResponse: () => void = () => undefined;
		const { session, handlers } = sessionWithHandler(() => ({
			...pendingPort(),
			call: <T>(
				method: string,
				_params: JsonValue,
				parse: (value: unknown) => T,
			): Promise<T> => {
				if (method !== "turn/start") {
					return Promise.reject(new Error(`unexpected call ${method}`));
				}
				beforeResponse();
				return Promise.resolve(
					parse({
						turn: { id: responseTurn, items: [], status: "inProgress" },
					}),
				);
			},
		}));
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
		beforeResponse = () => {
			handlers.onNotification(
				parsed("turn/started", {
					threadId: "thread-1",
					turn: {
						id: "turn-2",
						items: [],
						status: "inProgress",
						startedAt: 1_700_000_000,
					},
				}),
			);
			vi.setSystemTime(new Date(1_700_000_002_000));
		};
		await session.sendMessage("hello");
		const { container } = render(App, { props: { session } });
		await tick();
		const elapsed = () =>
			container.querySelector(".thread-head .tag.live .elapsed")?.textContent;
		expect(elapsed()).toBe("2s");

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
		expect(elapsed()).toBe("3s");

		handlers.onNotification(
			parsed("turn/completed", {
				threadId: "thread-1",
				turn: {
					id: "turn-2",
					items: [],
					status: "completed",
					startedAt: 1_700_000_000,
					completedAt: 1_700_000_004,
					durationMs: 4000,
				},
			}),
		);
		await tick();
		vi.setSystemTime(new Date(1_700_000_010_000));
		responseTurn = "turn-3";
		beforeResponse = () => undefined;
		await session.sendMessage("again");
		await tick();
		expect(elapsed()).toBe("");

		handlers.onNotification(
			parsed("turn/started", {
				threadId: "thread-1",
				turn: {
					id: "turn-3",
					items: [],
					status: "inProgress",
					startedAt: 1_700_000_010,
				},
			}),
		);
		await tick();
		expect(elapsed()).toBe("0 ms");
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

describe("status announcement", () => {
	afterEach(() => {
		cleanup();
	});

	function announcement(container: HTMLElement): string | null {
		return (
			container.querySelector(".notices[role=status] .sr-only")?.textContent ??
			null
		);
	}

	test("the status region follows the open turn", async () => {
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
		const { container } = render(App, { props: { session } });
		await tick();
		expect(announcement(container)).toBe("");

		handlers.onNotification(
			parsed("turn/started", {
				threadId: "thread-1",
				turn: { id: "turn-2", status: "inProgress", items: [] },
			}),
		);
		await tick();
		expect(announcement(container)).toBe("Working");

		handlers.onNotification(
			parsed("turn/completed", {
				threadId: "thread-1",
				turn: {
					id: "turn-2",
					items: [],
					status: "completed",
					startedAt: 1_700_000_000,
					completedAt: 1_700_000_004,
					durationMs: 4000,
				},
			}),
		);
		await tick();
		expect(announcement(container)).toBe("Done");
	});

	test("the status region carries the session review note while the turn runs", async () => {
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
		const { container } = render(App, { props: { session } });
		handlers.onNotification(
			parsed("turn/started", {
				threadId: "thread-1",
				turn: { id: "turn-2", status: "inProgress", items: [] },
			}),
		);
		await tick();
		expect(announcement(container)).toBe("Working");

		session.reviewNote = "Flagged: removed code";
		await tick();
		expect(announcement(container)).toBe("Flagged: removed code");

		session.reviewNote = null;
		await tick();
		expect(announcement(container)).toBe("Working");
	});
});

describe("open thread hue", () => {
	afterEach(() => {
		cleanup();
	});

	function twoThreads(): Session {
		const { session } = sessionWithHandler();
		session.connection = "connected";
		session.threads = [
			{
				...threadSummary("idle", []),
				id: "thread-1",
				title: "first",
				preview: "first",
			},
			{
				...threadSummary("idle", []),
				id: "thread-2",
				title: "second",
				preview: "second",
			},
		];
		return session;
	}

	function rowHue(container: HTMLElement, title: string): string | null {
		for (const row of container.querySelectorAll("button.thread")) {
			if (row.querySelector(".title")?.textContent === title) {
				return row.querySelector(".hue-dot")?.getAttribute("data-hue") ?? null;
			}
		}
		throw new Error(`no thread row ${title}`);
	}

	function openView(container: HTMLElement): HTMLElement {
		const main = container.querySelector<HTMLElement>("main");
		if (main === null) throw new Error("no open view");
		return main;
	}

	test("the open view carries the hue of the open thread's row", async () => {
		const session = twoThreads();
		const { container } = render(App, { props: { session } });
		const view = openView(container);
		const first = rowHue(container, "first");
		const second = rowHue(container, "second");
		expect(first).not.toBeNull();
		expect(second).not.toBeNull();
		expect(first).not.toBe(second);

		session.activeThreadId = "thread-1";
		await tick();
		expect(view.getAttribute("data-hue")).toBe(first);

		session.activeThreadId = "thread-2";
		await tick();
		expect(view.getAttribute("data-hue")).toBe(second);
	});

	test("a view with no thread open carries no hue", async () => {
		const session = twoThreads();
		const { container } = render(App, { props: { session } });
		const view = openView(container);
		expect(view.hasAttribute("data-hue")).toBe(false);

		session.activeThreadId = "thread-1";
		await tick();
		expect(view.hasAttribute("data-hue")).toBe(true);

		session.activeThreadId = null;
		await tick();
		expect(view.hasAttribute("data-hue")).toBe(false);
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

function sandboxQuestion(id: string, itemId: string): QuestionRequest {
	return {
		id,
		method: "item/tool/requestUserInput",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId,
		questions: [
			{
				id: "sandbox",
				header: "Sandbox",
				question: "How should commands in this thread run?",
				isOther: false,
				multiSelect: false,
				options: [
					{
						label: "Accept",
						description: "Run without a sandbox.",
						preview: null,
					},
					{
						label: "Dangerously accept",
						description: "Run without a sandbox under approval mode auto.",
						preview: null,
					},
				],
			},
		],
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
		handlers.onQuestion(sandboxQuestion("req-2", "sandbox-1"));
		render(App, { props: { session } });
		await tick();
		expect(
			screen.getByRole("heading", { name: "Apply these changes?" }),
		).toBeTruthy();
		expect(
			screen.queryByRole("heading", { name: "Answer this question" }),
		).toBeNull();

		handlers.onNotification(
			parsed("serverRequest/resolved", {
				threadId: "thread-1",
				requestId: "req-1",
			}),
		);
		await tick();
		expect(
			screen.getByRole("heading", { name: "Answer this question" }),
		).toBeTruthy();
	});
});

describe("keyboard paths", () => {
	afterEach(() => {
		cleanup();
	});

	test("reaches each control in order with Tab", async () => {
		const { session, handlers } = connectedSession();
		render(App, { props: { session } });
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
				turn: { id: "turn-2", status: "inProgress", items: [] },
			}),
		);
		handlers.onNotification(
			parsed("item/completed", {
				threadId: "thread-1",
				turnId: "turn-2",
				completedAtMs: 2,
				item: {
					type: "commandExecution",
					id: "run-1",
					command: "seq 3",
					aggregatedOutput: "1\n2\n3",
					status: "completed",
					exitCode: 0,
				},
			}),
		);
		handlers.onRequest(commandApproval("req-1", "cmd-1"));
		await tick();
		await fireEvent.scroll(stubTranscriptScroll());
		await tick();
		const user = userEvent.setup();
		const stops: HTMLElement[] = [];
		for (let index = 0; index < 40; index++) {
			await user.tab();
			if (document.activeElement instanceof HTMLElement) {
				stops.push(document.activeElement);
			}
		}
		const expected: readonly [string, (el: HTMLElement) => boolean][] = [
			["skip link", (el) => el.classList.contains("skip-link")],
			["Start thread", (el) => el.textContent?.trim() === "Start thread"],
			["thread", (el) => el.closest("nav") !== null && el.tagName === "BUTTON"],
			[
				"transcript",
				(el) =>
					el.closest("#transcript") !== null &&
					el.tagName === "BUTTON" &&
					!el.classList.contains("jump"),
			],
			["Jump to latest", (el) => el.classList.contains("jump")],
			[
				"Accept",
				(el) =>
					el.textContent?.trim() === "Accept" &&
					el.closest(".request-card") !== null,
			],
			["prompt", (el) => el.id === "prompt"],
			[
				"send",
				(el) =>
					el.closest(".composer") !== null &&
					el.getAttribute("type") === "submit",
			],
		];
		let cursor = -1;
		for (const [name, matches] of expected) {
			const index = stops.findIndex((el, at) => at > cursor && matches(el));
			expect(index, name).toBeGreaterThan(cursor);
			cursor = index;
		}
	});

	test("sends a message with Enter", () => {
		const { session } = connectedSession();
		const send = vi.spyOn(session, "sendMessage").mockResolvedValue(true);
		render(App, { props: { session } });
		typeInPrompt("ship it");
		fireEvent.keyDown(promptField(), { key: "Enter" });
		expect(send.mock.calls[0]?.[0]).toBe("ship it");
	});

	test("accepts an approval with Tab and Enter", async () => {
		const { session, handlers } = connectedSession();
		const respond = vi
			.spyOn(session, "respondApproval")
			.mockImplementation(() => undefined);
		render(App, { props: { session } });
		const request = commandApproval("req-1", "cmd-1");
		handlers.onRequest(request);
		await tick();
		const user = userEvent.setup();
		for (let index = 0; index < 40; index++) {
			await user.tab();
			const el = document.activeElement;
			if (
				el instanceof HTMLElement &&
				el.textContent?.trim() === "Accept" &&
				el.closest(".request-card") !== null
			) {
				break;
			}
		}
		await user.keyboard("{Enter}");
		expect(respond).toHaveBeenCalledWith(request, "accept");
	});

	test("opens and closes every panel from the keyboard", () => {
		const { session } = connectedSession();
		render(App, { props: { session } });
		fireEvent.keyDown(document.body, { key: "?" });
		expect(openDialogs()).toBe(1);
		fireEvent.keyDown(dialog(), { key: "Escape" });
		expect(openDialogs()).toBe(0);
		for (const command of slashCommands) {
			promptField().focus();
			typeInPrompt(`/${command.name}`);
			fireEvent.keyDown(promptField(), { key: "Enter" });
			expect(openDialogs(), command.name).toBe(1);
			fireEvent.keyDown(dialog(), { key: "Escape" });
			expect(openDialogs(), command.name).toBe(0);
		}
	});

	test("opens the review with Ctrl+O and closes it with Escape", async () => {
		const { session, handlers } = connectedSession();
		render(App, { props: { session } });
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
				turn: { id: "turn-2", status: "inProgress", items: [] },
			}),
		);
		handlers.onNotification(
			parsed("item/completed", {
				threadId: "thread-1",
				turnId: "turn-2",
				completedAtMs: 2,
				item: {
					type: "commandExecution",
					id: "run-1",
					command: "seq 30",
					aggregatedOutput: "line 1\nline 2",
					status: "completed",
					exitCode: 0,
				},
			}),
		);
		await tick();
		promptField().focus();
		fireEvent.keyDown(promptField(), { key: "o", ctrlKey: true });
		expect(openDialogs()).toBe(1);
		expect(dialog().textContent).toContain("seq 30");
		fireEvent.keyDown(dialog(), { key: "Escape" });
		expect(openDialogs()).toBe(0);
	});
});

describe("point of regard and tab stops", () => {
	afterEach(() => {
		cleanup();
	});

	function sidebarStart(container: HTMLElement): HTMLElement {
		const button = container.querySelector<HTMLElement>(
			".sidebar header button",
		);
		if (button === null) throw new Error("no sidebar start button");
		return button;
	}

	function shownPoints(container: HTMLElement): HTMLElement[] {
		return [...container.querySelectorAll<HTMLElement>(".point")].filter(
			(element) => element.closest("dialog:not([open])") === null,
		);
	}

	function runningSession(): { session: Session; handlers: RpcHandlers } {
		const { session, handlers } = connectedSession();
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
				turn: { id: "turn-2", status: "inProgress", items: [] },
			}),
		);
		return { session, handlers };
	}

	test("exactly one point per view", async () => {
		const empty = sessionWithHandler();
		empty.session.connection = "connected";
		const emptyView = render(App, { props: { session: empty.session } });
		await tick();
		const emptyPoints = shownPoints(emptyView.container);
		expect(emptyPoints).toHaveLength(1);
		expect(emptyPoints[0]?.textContent?.trim()).toBe("Start thread");
		expect(emptyPoints[0]?.closest(".empty-state")).not.toBeNull();
		expect(sidebarStart(emptyView.container).classList.contains("point")).toBe(
			false,
		);
		empty.session.stop();

		const idle = connectedSession();
		const idleView = render(App, { props: { session: idle.session } });
		await tick();
		const idlePoints = shownPoints(idleView.container);
		expect(idlePoints).toHaveLength(1);
		expect(idlePoints[0]?.closest(".composer")).not.toBeNull();
		expect(idlePoints[0]?.getAttribute("type")).toBe("submit");
		expect(sidebarStart(idleView.container).classList.contains("point")).toBe(
			false,
		);

		const running = runningSession();
		const runningView = render(App, { props: { session: running.session } });
		await tick();
		const runningPoints = shownPoints(runningView.container);
		expect(runningPoints).toHaveLength(1);
		expect(runningPoints[0]?.classList.contains("live")).toBe(true);
		expect(runningPoints[0]?.closest(".thread-head")).not.toBeNull();
		expect(
			sidebarStart(runningView.container).classList.contains("point"),
		).toBe(false);

		const deciding = connectedSession();
		deciding.handlers.onRequest(commandApproval("req-1", "cmd-1"));
		const decidingView = render(App, { props: { session: deciding.session } });
		await tick();
		const decidingPoints = shownPoints(decidingView.container);
		expect(decidingPoints).toHaveLength(1);
		expect(decidingPoints[0]?.textContent?.trim()).toBe("Accept");
		expect(decidingPoints[0]?.closest(".request-card")).not.toBeNull();
		expect(
			sidebarStart(decidingView.container).classList.contains("point"),
		).toBe(false);
	});

	test("no scroll box takes a tab stop", async () => {
		const { session, handlers } = runningSession();
		session.threads = Array.from({ length: 40 }, (_, index) => ({
			...threadSummary("idle", []),
			id: `thread-${index + 1}`,
			title: `thread ${index + 1}`,
			preview: `thread ${index + 1}`,
		}));
		for (const at of [1, 2, 3, 4, 5]) {
			handlers.onNotification(
				parsed("item/completed", {
					threadId: "thread-1",
					turnId: "turn-2",
					completedAtMs: at,
					item: {
						type: "commandExecution",
						id: `run-${at}`,
						command: `seq ${at}`,
						aggregatedOutput: Array.from(
							{ length: 12 },
							(_, line) => `line ${line + 1}`,
						).join("\n"),
						status: "completed",
						exitCode: 0,
					},
				}),
			);
		}
		handlers.onRequest({
			...commandApproval("req-1", "cmd-1"),
			detail:
				"git log --oneline --decorate --graph --all --stat --summary --follow",
		});
		const { container } = render(App, { props: { session } });
		await tick();
		const stops: HTMLElement[] = [];
		const user = userEvent.setup();
		if (document.activeElement instanceof HTMLElement) {
			document.activeElement.blur();
		}
		for (let attempt = 0; attempt < 200; attempt++) {
			await user.tab();
			const active = document.activeElement;
			if (!(active instanceof HTMLElement) || active === document.body) break;
			if (stops.length > 0 && active === stops[0]) break;
			stops.push(active);
		}
		expect(stops.length).toBeGreaterThan(0);
		const stopSelector = "a[href], button, input, select, textarea, summary";
		const boxes = [
			container.querySelector("nav"),
			container.querySelector("#transcript"),
			...container.querySelectorAll('[role="region"], .dock'),
		];
		const threadButtons = [
			...container.querySelectorAll<HTMLElement>("nav button.thread"),
		];
		expect(threadButtons).toHaveLength(40);
		const reached = new Set(stops);
		for (const button of threadButtons) {
			expect(reached.has(button), button.textContent ?? "").toBe(true);
		}
		for (const stop of stops) {
			expect(stop.matches(stopSelector), stop.className).toBe(true);
			for (const box of boxes) {
				expect(stop === box).toBe(false);
			}
		}
	});
});

function connectedSession(): { session: Session; handlers: RpcHandlers } {
	const { session, handlers } = sessionWithHandler(pendingPort);
	session.connection = "connected";
	session.activeThreadId = "thread-1";
	session.threads = [threadSummary("idle", [])];
	return { session, handlers };
}

function promptField(): HTMLTextAreaElement {
	const field = document.getElementById("prompt");
	if (!(field instanceof HTMLTextAreaElement)) {
		throw new Error("no prompt field");
	}
	return field;
}

function typeInPrompt(text: string): void {
	const field = promptField();
	field.focus();
	fireEvent.input(field, { target: { value: text } });
}

function dialog(): HTMLDialogElement {
	const found = document.querySelector("dialog[open]");
	if (!(found instanceof HTMLDialogElement)) {
		throw new Error("no open dialog");
	}
	return found;
}

function openDialogs(): number {
	return document.querySelectorAll("dialog[open]").length;
}

function stubTranscriptScroll(): HTMLElement {
	const transcript = document.getElementById("transcript");
	if (transcript === null) throw new Error("transcript section missing");
	transcript.style.overflowY = "auto";
	Object.defineProperty(transcript, "scrollHeight", {
		configurable: true,
		value: 2000,
	});
	Object.defineProperty(transcript, "clientHeight", {
		configurable: true,
		value: 500,
	});
	Object.defineProperty(transcript, "scrollTop", {
		configurable: true,
		writable: true,
		value: 0,
	});
	Object.defineProperty(transcript, "scrollTo", {
		configurable: true,
		value: vi.fn(),
	});
	return transcript;
}

describe("dock height", () => {
	afterEach(() => {
		Reflect.deleteProperty(HTMLElement.prototype, "clientHeight");
		cleanup();
	});

	test("publishes the dock's height so Jump to latest sits above it", async () => {
		Object.defineProperty(HTMLElement.prototype, "clientHeight", {
			configurable: true,
			get(this: HTMLElement) {
				return this.classList.contains("dock") ? 240 : 0;
			},
		});
		const dial = (_url: string, _handlers: RpcHandlers): RpcPort => idlePort();
		render(App, { props: { session: new Session(dial) } });
		await tick();
		const shell = document.querySelector<HTMLElement>(".shell");
		expect(shell?.style.getPropertyValue("--dock-height")).toBe("240px");
	});
});

describe("page live regions", () => {
	afterEach(() => {
		cleanup();
	});

	const liveSelector =
		"[role=alert], [role=status], [role=log], [role=timer], [aria-live]:not([aria-live=off]), output";
	const composerSelector = '[aria-label="Message composer"]';

	test("the page has one status region and one alert region while a panel and the composer show their own errors", async () => {
		const { session } = connectedSession();
		vi.spyOn(session, "compactThread").mockRejectedValue(
			new Error("compact refused"),
		);
		render(App, { props: { session } });
		await tick();
		fireEvent.keyDown(promptField(), { key: "Enter" });
		await tick();
		session.openPanel("compact");
		await tick();
		fireEvent.click(screen.getByRole("button", { name: "Compact" }));
		await screen.findByText("compact refused");
		expect(screen.getByText("Enter a message before sending.")).toBeTruthy();

		const live = [...document.querySelectorAll<HTMLElement>(liveSelector)];
		const page = live.filter(
			(node) =>
				node.closest("dialog") === null &&
				node.closest(composerSelector) === null,
		);
		expect(page.map((node) => node.getAttribute("role"))).toEqual([
			"alert",
			"status",
		]);
		expect(page.every((node) => node.classList.contains("notices"))).toBe(true);
		expect(page[0]?.querySelectorAll(".notice")).toHaveLength(0);
		expect(session.notices).toHaveLength(0);

		const panel = live.filter((node) => node.closest("dialog") !== null);
		expect(panel.map((node) => node.textContent)).toEqual(["compact refused"]);
		expect(panel.map((node) => node.getAttribute("role"))).toEqual(["alert"]);

		const composer = live.filter(
			(node) => node.closest(composerSelector) !== null,
		);
		expect(composer.map((node) => node.id)).toEqual(["composer-error"]);
		expect(composer[0]?.textContent).toBe("Enter a message before sending.");
		expect(promptField().getAttribute("aria-describedby")).toContain(
			"composer-error",
		);

		const transcript = document.getElementById("transcript");
		expect(transcript).not.toBeNull();
		expect(transcript?.matches(liveSelector)).toBe(false);
		expect(transcript?.querySelector(liveSelector)).toBeNull();
		session.stop();
	});
});
