import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import App from "#lib/components/App.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type { ApprovalRequest, JsonValue, RpcHandlers } from "#lib/types.ts";
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
	session.approvals = [request];
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
		expect(container.querySelector(".status-bar")?.textContent).toContain(
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
		expect(container.querySelector(".status-bar")?.textContent).toContain("3s");
		session.stop();
	});
});
