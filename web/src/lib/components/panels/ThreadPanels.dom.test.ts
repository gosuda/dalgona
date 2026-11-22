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

function recordingPort(answers: Record<string, Answer>): {
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
			return Promise.resolve(parse(answer(params)));
		},
		notify: (): void => undefined,
		respond: (): void => undefined,
		open: () => Promise.resolve(),
		close: (): void => undefined,
	};
	return { port, calls };
}

function activeSession(answers: Record<string, Answer>): {
	session: Session;
	calls: Call[];
} {
	const { port, calls } = recordingPort(answers);
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
