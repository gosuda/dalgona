import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { tick } from "svelte";
import { afterEach, describe, expect, test } from "vitest";
import App from "#lib/components/App.svelte";
import ThreadPanels from "#lib/components/panels/ThreadPanels.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type { JsonValue, RpcHandlers } from "#lib/types.ts";

afterEach(() => {
	cleanup();
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
		expect(
			screen.getByText(
				"/compact is unavailable while a turn is working; wait for it to finish or interrupt it first",
			),
		).toBeTruthy();

		const user = userEvent.setup();
		await user.click(screen.getByRole("button", { name: "Interrupt" }));
		await tick();
		expect(calls.map((call) => call.method)).toContain("turn/interrupt");
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
