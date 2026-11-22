import {
	cleanup,
	fireEvent,
	render,
	screen,
	within,
} from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import App from "#lib/components/App.svelte";
import ResumePanel from "#lib/components/panels/ResumePanel.svelte";
import ThreadPanels from "#lib/components/panels/ThreadPanels.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type { JsonValue, RpcHandlers, ThreadListResult } from "#lib/types.ts";

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	document.body.replaceChildren();
});

type Answer = (params: JsonValue) => JsonValue;
type Call = { readonly method: string; readonly params: JsonValue };

function recordingPort(
	answers: Record<string, Answer>,
	held: Record<string, Promise<void>> = {},
): {
	port: RpcPort;
	calls: Call[];
} {
	const calls: Call[] = [];
	const port: RpcPort = {
		call: <T>(
			method: string,
			params: JsonValue,
			parse: (value: unknown) => T,
		): Promise<T> => {
			calls.push({ method, params });
			const answer = answers[method];
			if (answer === undefined) {
				return Promise.reject(new Error(`no answer for ${method}`));
			}
			const hold = held[method];
			if (hold !== undefined) {
				return hold.then(() => parse(answer(params)));
			}
			return Promise.resolve(parse(answer(params)));
		},
		notify: (): void => undefined,
		respond: (): void => undefined,
		open: () => Promise.resolve(),
		close: (): void => undefined,
	};
	return { port, calls };
}

function activeSession(
	answers: Record<string, Answer>,
	held: Record<string, Promise<void>> = {},
): {
	session: Session;
	calls: Call[];
} {
	const { port, calls } = recordingPort(answers, held);
	const session = new Session((_url: string, _handlers: RpcHandlers) => port);
	session.start({
		hash: "#token=abc",
		host: "localhost:7437",
		pathname: "/",
		protocol: "http:",
		search: "",
	});
	session.activeThreadId = "thread-1";
	return { session, calls };
}

const tree: Answer = () => ({
	turns: [
		{
			turnId: "turn-1",
			parentTurnId: null,
			preview: "first prompt",
			active: true,
		},
		{
			turnId: "turn-2",
			parentTurnId: "turn-1",
			preview: "second prompt",
			active: true,
		},
	],
});

describe("thread command panels", () => {
	test.each(["sidebar", "sidebar with no thread open", "resume panel"])(
		"opens a thread beyond the first 100 from the %s",
		async (view) => {
			const row = (id: number) => ({
				id: String(id),
				title: `Thread ${id}`,
				preview: "",
				status: "idle" as const,
				updatedAt: 1,
				model: null,
				cwd: "/work",
			});
			const firstPage = Array.from({ length: 100 }, (_, index) =>
				row(index + 1),
			);
			const older = { ...row(101), title: "Older thread" };
			const session = new Session(vi.fn());
			session.connection = "connected";
			if (view !== "sidebar with no thread open") session.activeThreadId = "1";
			session.threads = firstPage;
			session.nextThreadCursor = "older";
			const pending = Promise.withResolvers<ThreadListResult>();
			const page = vi
				.spyOn(session, "listThreads")
				.mockImplementation(async (cursor) =>
					cursor === "older"
						? pending.promise
						: { threads: firstPage, nextCursor: "older" },
				);
			const select = vi
				.spyOn(session, "selectThread")
				.mockResolvedValue(undefined);
			render(App, { props: { session } });
			if (view === "resume panel") session.openPanel("resume");
			const list = within(
				await (view === "resume panel"
					? screen.findByRole("dialog", { name: "Open an earlier thread" })
					: screen.findByRole("navigation", { name: "Conversation threads" })),
			);
			expect(list.queryByText("Older thread")).toBeNull();
			const more = await list.findByRole("button", {
				name: "Load more threads",
			});
			const user = userEvent.setup();
			more.focus();
			await user.keyboard("{Enter}");
			expect(document.activeElement).toBe(more);
			expect(more.getAttribute("aria-disabled")).toBe("true");
			expect(more.hasAttribute("disabled")).toBe(false);
			await user.keyboard(" ");
			expect(
				page.mock.calls.filter(([cursor]) => cursor === "older"),
			).toHaveLength(1);
			pending.resolve({ threads: [row(100), older], nextCursor: null });
			const choice = await list.findByText("Older thread");
			const fallback =
				view === "resume panel"
					? list.getByRole("searchbox")
					: document.getElementById(
							view === "sidebar" ? "prompt" : "transcript",
						);
			expect(fallback).toBeTruthy();
			expect(document.activeElement).toBe(fallback);
			expect(list.getAllByRole("listitem")).toHaveLength(101);
			expect(
				list.queryByRole("button", { name: "Load more threads" }),
			).toBeNull();
			expect(select).not.toHaveBeenCalled();
			await fireEvent.click(choice);
			expect(select).toHaveBeenCalledWith("101");
			expect(session.panel).toBeNull();
		},
	);

	test.each([
		{ panel: "compact", action: "Compact", method: "thread/compact/start" },
		{ panel: "review", action: "Start review", method: "review/start" },
		{ panel: "rewind", action: "Rewind", method: "thread/revert" },
		{
			panel: "tree",
			action: "Switch branch",
			method: "dalgona/thread/leaf/set",
		},
		{ panel: "fork", action: "Fork here", method: "thread/fork" },
		{
			panel: "advisor",
			action: "Turn advisor on",
			method: "dalgona/advisor/set",
		},
		{
			panel: "advisor",
			action: "Reset advisor",
			method: "dalgona/advisor/reset",
		},
	] as const)(
		"$action stays focusable, reports aria-disabled and sends one request while it runs",
		async ({ panel, action, method }) => {
			const answer = Promise.withResolvers<void>();
			const { session, calls } = activeSession(
				{ "dalgona/thread/tree": tree, [method]: () => ({}) },
				{ [method]: answer.promise },
			);
			render(ThreadPanels, { props: { session } });
			session.openPanel(panel, panel === "rewind" ? "turn-2" : undefined);
			const user = userEvent.setup();
			if (panel === "fork") {
				await user.click(await screen.findByLabelText(/first prompt/));
			}
			const button = (await screen.findByRole("button", {
				name: action,
			})) as HTMLButtonElement;
			await vi.waitFor(() => expect(button.disabled).toBe(false));
			const sent = () => calls.filter((call) => call.method === method);
			button.focus();
			await user.keyboard("{Enter}");
			expect(sent()).toHaveLength(1);
			expect(document.activeElement).toBe(button);
			expect(button.getAttribute("aria-disabled")).toBe("true");
			expect(button.hasAttribute("disabled")).toBe(false);
			await user.keyboard("{Enter}");
			await user.click(button);
			expect(sent()).toHaveLength(1);
			answer.reject(new Error("refused"));
			await vi.waitFor(() =>
				expect(button.hasAttribute("aria-disabled")).toBe(false),
			);
			expect(session.panel).toBe(panel);
		},
	);

	test("rewind asks before calling thread/revert", async () => {
		const { session, calls } = activeSession({
			"dalgona/thread/tree": tree,
			"thread/revert": () => ({ thread: {} }),
			"thread/resume": () => ({}),
		});
		render(ThreadPanels, { props: { session } });
		session.openPanel("rewind", "turn-2");
		await tick();
		await tick();

		expect(
			screen.getByText(
				"Rewind to before this turn? Later turns leave the active path. Files changed by later turns are not reverted.",
			),
		).toBeTruthy();
		expect(calls.map((call) => call.method)).not.toContain("thread/revert");

		const user = userEvent.setup();
		await user.click(screen.getByRole("button", { name: "Rewind" }));
		await tick();
		expect(calls).toContainEqual({
			method: "thread/revert",
			params: { threadId: "thread-1", beforeTurnId: "turn-2" },
		});
	});

	test("compact is blocked mid-turn with an Interrupt action", async () => {
		const { session, calls } = activeSession({
			"turn/interrupt": () => ({}),
		});
		render(App, { props: { session } });
		session.transcript = {
			...session.transcript,
			turnStatus: "inProgress",
			turnId: "turn-1",
		};
		session.openPanel("compact");
		await tick();

		expect(session.panel).toBeNull();
		const notice = screen.getByText(
			"/compact is unavailable while a turn is working; wait for it to finish or interrupt it first",
		);

		const user = userEvent.setup();
		await user.click(
			within(notice.parentElement as HTMLElement).getByRole("button", {
				name: "Interrupt",
			}),
		);
		await tick();
		expect(calls).toContainEqual({
			method: "turn/interrupt",
			params: { threadId: "thread-1", turnId: "turn-1" },
		});
	});

	test("review/start sends the chosen target", async () => {
		const { session, calls } = activeSession({
			"review/start": () => ({ reviewThreadId: "thread-1", turn: {} }),
		});
		render(ThreadPanels, { props: { session } });
		session.openPanel("review");
		await tick();

		const user = userEvent.setup();
		await user.click(screen.getByRole("radio", { name: "Base branch" }));
		await user.type(
			screen.getByRole("textbox", { name: "Base branch name" }),
			"main",
		);
		await user.click(screen.getByRole("button", { name: "Start review" }));
		await tick();
		expect(calls).toContainEqual({
			method: "review/start",
			params: {
				threadId: "thread-1",
				target: { type: "baseBranch", branch: "main" },
			},
		});
	});

	test("fork clears a stale turn selection when the panel reopens", async () => {
		const answers: Record<string, Answer> = { "dalgona/thread/tree": tree };
		const { session } = activeSession(answers);
		render(ThreadPanels, { props: { session } });
		session.openPanel("fork");
		const user = userEvent.setup();
		await user.click(await screen.findByLabelText(/first prompt/));
		expect(
			(screen.getByRole("button", { name: "Fork here" }) as HTMLButtonElement)
				.disabled,
		).toBe(false);

		session.closePanel();
		await tick();
		answers["dalgona/thread/tree"] = () => ({
			turns: [
				{
					turnId: "turn-3",
					parentTurnId: null,
					preview: "third prompt",
					active: true,
				},
			],
		});
		session.openPanel("fork");
		await screen.findByLabelText(/third prompt/);
		expect(
			(screen.getByRole("button", { name: "Fork here" }) as HTMLButtonElement)
				.disabled,
		).toBe(true);
	});
});

test("searches conversation text after a pause and ignores cancelled results", async () => {
	vi.useFakeTimers();
	let finishOld: (
		value: readonly {
			threadId: string;
			name: string;
			preview: string;
			snippet: string;
			matches: number;
		}[],
	) => void = () => {};
	const search = vi
		.fn()
		.mockImplementationOnce(
			() =>
				new Promise((resolve) => {
					finishOld = resolve;
				}),
		)
		.mockResolvedValue([
			{
				threadId: "found",
				name: "Deployment",
				preview: "",
				snippet: "needle in conversation",
				matches: 1,
			},
		]);
	const select = vi.fn().mockResolvedValue(undefined);
	const close = vi.fn();
	const session = {
		listThreads: vi.fn().mockResolvedValue({ threads: [], nextCursor: null }),
		searchThreads: search,
		selectThread: select,
		closePanel: close,
		openedPanel: 1,
		closePanelIf: (opened: number) => {
			if (opened === 1) close();
		},
	} as unknown as Session;
	render(ResumePanel, { session, open: true });
	const input = screen.getByRole("searchbox", { name: "Filter threads" });
	await fireEvent.input(input, { target: { value: "ab" } });
	await vi.advanceTimersByTimeAsync(300);
	expect(search).not.toHaveBeenCalled();
	await fireEvent.input(input, { target: { value: "old" } });
	await vi.advanceTimersByTimeAsync(299);
	expect(search).not.toHaveBeenCalled();
	await vi.advanceTimersByTimeAsync(1);
	expect(search).toHaveBeenCalledTimes(1);
	const oldSignal = search.mock.calls[0]?.[1] as AbortSignal;
	await fireEvent.input(input, { target: { value: "need" } });
	await fireEvent.input(input, { target: { value: "needle" } });
	expect(oldSignal.aborted).toBe(true);
	await vi.advanceTimersByTimeAsync(300);
	expect(search).toHaveBeenCalledTimes(2);
	await tick();
	finishOld([
		{
			threadId: "stale",
			name: "Stale result",
			preview: "",
			snippet: "old text",
			matches: 1,
		},
	]);
	await tick();
	expect(screen.queryByText("Stale result")).toBeNull();
	await fireEvent.click(
		screen.getByRole("button", { name: /Deployment.*needle in conversation/ }),
	);
	expect(select).toHaveBeenCalledWith("found");
	expect(close).toHaveBeenCalledOnce();
});

test("conversation search offers retry and distinguishes no matches", async () => {
	vi.useFakeTimers();
	const search = vi
		.fn()
		.mockRejectedValueOnce(new Error("offline"))
		.mockResolvedValue([]);
	const session = {
		listThreads: vi.fn().mockResolvedValue({ threads: [], nextCursor: null }),
		searchThreads: search,
		closePanel: vi.fn(),
		openedPanel: 1,
		closePanelIf: vi.fn(),
	} as unknown as Session;
	render(ResumePanel, { session, open: true });
	await fireEvent.input(screen.getByRole("searchbox"), {
		target: { value: "needle" },
	});
	await vi.advanceTimersByTimeAsync(300);
	await tick();
	expect(screen.getByRole("alert").textContent).toBe("Search failed: offline.");
	await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
	await vi.advanceTimersByTimeAsync(300);
	await tick();
	expect(screen.getByText("No conversation mentions this text.")).toBeTruthy();
});

test("resume closes after the thread loads and focus lands in the enabled message field", async () => {
	const prompt = document.createElement("textarea");
	prompt.id = "prompt";
	document.body.append(prompt);
	prompt.focus();
	let finish: () => void = () => {};
	const select = vi.fn().mockImplementation(() => {
		prompt.disabled = true;
		return new Promise<void>((resolve) => {
			finish = () => {
				prompt.disabled = false;
				resolve();
			};
		});
	});
	let view: ReturnType<typeof render> | undefined;
	const close = vi.fn(() => void view?.rerender({ session, open: false }));
	const session = {
		listThreads: vi.fn().mockResolvedValue({
			threads: [
				{
					id: "thread-2",
					title: "hello",
					preview: "",
					status: "idle",
					updatedAt: 1,
					model: null,
					cwd: "/work",
				},
			],
			nextCursor: null,
		}),
		searchThreads: vi.fn().mockResolvedValue([]),
		selectThread: select,
		closePanel: close,
		openedPanel: 1,
		closePanelIf: (opened: number) => {
			if (opened === 1) close();
		},
	} as unknown as Session;
	view = render(ResumePanel, { session, open: true });
	await tick();
	await fireEvent.click(await screen.findByRole("button", { name: "hello" }));
	expect(select).toHaveBeenCalledWith("thread-2");
	expect(close).not.toHaveBeenCalled();
	expect(document.querySelector("dialog[open]")).not.toBeNull();
	finish();
	await vi.waitFor(() => expect(close).toHaveBeenCalledOnce());
	await tick();
	expect(document.querySelector("dialog[open]")).toBeNull();
	expect(prompt.disabled).toBe(false);
	expect(document.activeElement).toBe(prompt);
});

test("resume keeps focus on the chosen thread, shows one status line and takes no second choice while it loads", async () => {
	let finish: () => void = () => {};
	const select = vi.fn().mockImplementation(
		() =>
			new Promise<void>((resolve) => {
				finish = resolve;
			}),
	);
	let view: ReturnType<typeof render> | undefined;
	const close = vi.fn(() => void view?.rerender({ session, open: false }));
	const session = {
		listThreads: vi.fn().mockResolvedValue({
			threads: [
				{
					id: "thread-2",
					title: "hello",
					preview: "",
					status: "idle",
					updatedAt: 1,
					model: null,
					cwd: "/work",
				},
			],
			nextCursor: null,
		}),
		searchThreads: vi.fn().mockResolvedValue([]),
		selectThread: select,
		closePanel: close,
		openedPanel: 1,
		closePanelIf: (opened: number) => {
			if (opened === 1) close();
		},
	} as unknown as Session;
	view = render(ResumePanel, { session, open: true });
	await tick();
	const choice = (await screen.findByRole("button", {
		name: "hello",
	})) as HTMLButtonElement;
	expect(screen.queryByRole("status")).toBeNull();
	const user = userEvent.setup();
	choice.focus();
	await user.keyboard("{Enter}");
	expect(screen.getByRole("status").textContent).toBe("Opening thread…");
	expect(document.activeElement).toBe(choice);
	expect(choice.getAttribute("aria-disabled")).toBe("true");
	expect(choice.disabled).toBe(false);
	await user.keyboard("{Enter}");
	await user.click(choice);
	expect(select).toHaveBeenCalledTimes(1);
	finish();
	await vi.waitFor(() => expect(close).toHaveBeenCalledOnce());
	await tick();
	await view.rerender({ session, open: true });
	await tick();
	expect(screen.queryByRole("status")).toBeNull();
	const again = (await screen.findByRole("button", {
		name: "hello",
	})) as HTMLButtonElement;
	expect(again.disabled).toBe(false);
	expect(again.hasAttribute("aria-disabled")).toBe(false);
});

test("a resume that ends after Escape leaves the next panel and its sign-in open", async () => {
	const { session, calls } = activeSession({
		"account/login/cancel": () => ({}),
	});
	session.connection = "connected";
	vi.spyOn(session, "listThreads").mockResolvedValue({
		threads: [
			{
				id: "thread-2",
				title: "hello",
				preview: "",
				status: "idle",
				updatedAt: 1,
				model: null,
				cwd: "/work",
			},
		],
		nextCursor: null,
	});
	const pending = Promise.withResolvers<void>();
	const select = vi
		.spyOn(session, "selectThread")
		.mockReturnValue(pending.promise);
	render(ThreadPanels, { props: { session } });
	session.openPanel("resume");
	const dialog = await screen.findByRole("dialog", {
		name: "Open an earlier thread",
	});
	await fireEvent.click(
		await within(dialog).findByRole("button", { name: "hello" }),
	);
	expect(select).toHaveBeenCalledWith("thread-2");
	await fireEvent.keyDown(dialog, { key: "Escape" });
	expect(session.panel).toBeNull();
	session.openPanel("login");
	session.login = {
		provider: "chatgpt",
		method: "browser",
		loginId: "login-1",
		authUrl: "https://auth.example/start",
		verificationUrl: null,
		userCode: null,
		error: null,
		pending: true,
	};
	pending.resolve();
	await pending.promise;
	await tick();
	await tick();
	expect(session.panel).toBe("login");
	expect(session.login?.loginId).toBe("login-1");
	expect(calls.map((call) => call.method)).not.toContain(
		"account/login/cancel",
	);
});

test("a resume that ends after Escape leaves a reopened resume panel open", async () => {
	const { session } = activeSession({});
	session.connection = "connected";
	vi.spyOn(session, "listThreads").mockResolvedValue({
		threads: [
			{
				id: "thread-2",
				title: "hello",
				preview: "",
				status: "idle",
				updatedAt: 1,
				model: null,
				cwd: "/work",
			},
		],
		nextCursor: null,
	});
	const pending = Promise.withResolvers<void>();
	vi.spyOn(session, "selectThread").mockReturnValue(pending.promise);
	render(ThreadPanels, { props: { session } });
	session.openPanel("resume");
	const dialog = await screen.findByRole("dialog", {
		name: "Open an earlier thread",
	});
	await fireEvent.click(
		await within(dialog).findByRole("button", { name: "hello" }),
	);
	await fireEvent.keyDown(dialog, { key: "Escape" });
	expect(session.panel).toBeNull();
	session.openPanel("resume");
	await tick();
	pending.resolve();
	await pending.promise;
	await tick();
	await tick();
	expect(session.panel).toBe("resume");
});

test("a resume panel reopened during an earlier attempt starts enabled and keeps its own attempt", async () => {
	const { session } = activeSession({});
	session.connection = "connected";
	vi.spyOn(session, "listThreads").mockResolvedValue({
		threads: [
			{
				id: "thread-2",
				title: "hello",
				preview: "",
				status: "idle",
				updatedAt: 1,
				model: null,
				cwd: "/work",
			},
		],
		nextCursor: null,
	});
	const earlier = Promise.withResolvers<void>();
	const later = Promise.withResolvers<void>();
	vi.spyOn(session, "selectThread")
		.mockReturnValueOnce(earlier.promise)
		.mockReturnValueOnce(later.promise);
	render(ThreadPanels, { props: { session } });
	session.openPanel("resume");
	const dialog = await screen.findByRole("dialog", {
		name: "Open an earlier thread",
	});
	await fireEvent.click(
		await within(dialog).findByRole("button", { name: "hello" }),
	);
	expect(within(dialog).getByRole("status").textContent).toBe(
		"Opening thread…",
	);
	await fireEvent.keyDown(dialog, { key: "Escape" });
	expect(session.panel).toBeNull();
	session.openPanel("resume");
	await tick();
	const reopened = await screen.findByRole("dialog", {
		name: "Open an earlier thread",
	});
	const choice = (await within(reopened).findByRole("button", {
		name: "hello",
	})) as HTMLButtonElement;
	expect(within(reopened).queryByRole("status")).toBeNull();
	expect(choice.getAttribute("aria-disabled")).toBeNull();
	await fireEvent.click(choice);
	expect(within(reopened).getByRole("status").textContent).toBe(
		"Opening thread…",
	);
	earlier.resolve();
	await earlier.promise;
	await tick();
	await tick();
	expect(session.panel).toBe("resume");
	expect(within(reopened).getByRole("status").textContent).toBe(
		"Opening thread…",
	);
	expect(choice.getAttribute("aria-disabled")).toBe("true");
	expect(choice.disabled).toBe(false);
	later.resolve();
	await later.promise;
	await vi.waitFor(() => expect(session.panel).toBeNull());
});

type PanelAction = {
	readonly kind: "fork" | "tree" | "rewind" | "compact" | "review";
	readonly title: string;
	readonly method:
		| "forkThread"
		| "setLeaf"
		| "rewindTo"
		| "compactThread"
		| "startReview";
	readonly turnId: string | null;
	readonly act: (dialog: HTMLElement) => Promise<void>;
};

const panelActions: readonly PanelAction[] = [
	{
		kind: "fork",
		title: "Fork at a turn",
		method: "forkThread",
		turnId: null,
		act: async (dialog) => {
			await fireEvent.click(
				await within(dialog).findByLabelText(/first prompt/),
			);
			await fireEvent.click(
				within(dialog).getByRole("button", { name: "Fork here" }),
			);
		},
	},
	{
		kind: "tree",
		title: "Move to another branch",
		method: "setLeaf",
		turnId: null,
		act: async (dialog) => {
			await within(dialog).findByLabelText(/first prompt/);
			await tick();
			await fireEvent.click(
				within(dialog).getByRole("button", { name: "Switch branch" }),
			);
		},
	},
	{
		kind: "rewind",
		title: "Rewind to a turn",
		method: "rewindTo",
		turnId: "turn-2",
		act: async (dialog) => {
			await within(dialog).findByLabelText(/second prompt/);
			await tick();
			await fireEvent.click(
				within(dialog).getByRole("button", { name: "Rewind" }),
			);
		},
	},
	{
		kind: "compact",
		title: "Compact the context",
		method: "compactThread",
		turnId: null,
		act: async (dialog) => {
			await fireEvent.click(
				within(dialog).getByRole("button", { name: "Compact" }),
			);
		},
	},
	{
		kind: "review",
		title: "Review changes",
		method: "startReview",
		turnId: null,
		act: async (dialog) => {
			await fireEvent.click(
				within(dialog).getByRole("button", { name: "Start review" }),
			);
		},
	},
];

describe("an action that ends after its panel closed MUST NOT close the panel that is open then", () => {
	test.each(panelActions)(
		"$kind leaves the next panel open",
		async ({ kind, title, method, turnId, act }) => {
			const { session } = activeSession({ "dalgona/thread/tree": tree });
			const pending = Promise.withResolvers<void>();
			const action = vi.spyOn(session, method).mockReturnValue(pending.promise);
			render(ThreadPanels, { props: { session } });
			session.openPanel(kind, turnId);
			const dialog = await screen.findByRole("dialog", { name: title });
			await act(dialog);
			expect(action).toHaveBeenCalled();
			await fireEvent.keyDown(dialog, { key: "Escape" });
			expect(session.panel).toBeNull();
			session.openPanel("new");
			await tick();
			pending.resolve();
			await pending.promise;
			await tick();
			await tick();
			expect(session.panel).toBe("new");
		},
	);

	test.each(panelActions)(
		"$kind closes its own panel when it ends",
		async ({ kind, title, method, turnId, act }) => {
			const { session } = activeSession({ "dalgona/thread/tree": tree });
			const pending = Promise.withResolvers<void>();
			vi.spyOn(session, method).mockReturnValue(pending.promise);
			render(ThreadPanels, { props: { session } });
			session.openPanel(kind, turnId);
			const dialog = await screen.findByRole("dialog", { name: title });
			await act(dialog);
			expect(session.panel).toBe(kind);
			pending.resolve();
			await pending.promise;
			await tick();
			await tick();
			expect(session.panel).toBeNull();
		},
	);

	test.each([
		{
			kind: "model" as const,
			closedFirst: true,
			save: (session: Session) => session.saveModel("gpt-next", null),
		},
		{
			kind: "model" as const,
			closedFirst: false,
			save: (session: Session) => session.saveModel("gpt-next", null),
		},
		{
			kind: "settings" as const,
			closedFirst: true,
			save: (session: Session) =>
				session.saveSettings([{ keyPath: "verbosity", value: "high" }]),
		},
		{
			kind: "settings" as const,
			closedFirst: false,
			save: (session: Session) =>
				session.saveSettings([{ keyPath: "verbosity", value: "high" }]),
		},
	])(
		"saving $kind with its panel closed first: $closedFirst",
		async ({ kind, closedFirst, save }) => {
			const written = Promise.withResolvers<JsonValue>();
			const { port } = recordingPort({});
			const call = port.call;
			port.call = <T>(
				method: string,
				params: JsonValue,
				parse: (value: unknown) => T,
			): Promise<T> =>
				method === "config/batchWrite"
					? written.promise.then(parse)
					: call(method, params, parse);
			const session = new Session(() => port);
			session.start({
				hash: "#token=abc",
				host: "localhost:7437",
				pathname: "/",
				protocol: "http:",
				search: "",
			});
			session.openPanel(kind);
			const saving = save(session);
			if (closedFirst) {
				session.closePanel();
				session.openPanel("new");
			}
			written.resolve({});
			expect(await saving).toBe(true);
			expect(session.panel).toBe(closedFirst ? "new" : null);
			session.stop();
		},
	);

	test.each([
		{
			kind: "model" as const,
			next: "login" as const,
			save: (session: Session) => session.saveModel("gpt-next", null),
		},
		{
			kind: "settings" as const,
			next: "keys" as const,
			save: (session: Session) =>
				session.saveSettings([{ keyPath: "verbosity", value: "high" }]),
		},
	])(
		"saving $kind and then opening $next from it leaves $next open",
		async ({ kind, next, save }) => {
			const written = Promise.withResolvers<JsonValue>();
			const { port } = recordingPort({});
			const call = port.call;
			port.call = <T>(
				method: string,
				params: JsonValue,
				parse: (value: unknown) => T,
			): Promise<T> =>
				method === "config/batchWrite"
					? written.promise.then(parse)
					: call(method, params, parse);
			const session = new Session(() => port);
			session.start({
				hash: "#token=abc",
				host: "localhost:7437",
				pathname: "/",
				protocol: "http:",
				search: "",
			});
			session.openPanel(kind);
			const saving = save(session);
			session.openPanel(next);
			written.resolve({});
			expect(await saving).toBe(true);
			expect(session.panel).toBe(next);
			session.stop();
		},
	);
});
