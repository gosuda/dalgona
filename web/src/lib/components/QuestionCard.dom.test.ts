import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import App from "#lib/components/App.svelte";
import QuestionCard from "#lib/components/QuestionCard.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type {
	JsonValue,
	QuestionRequest,
	QuestionSelection,
	RequestId,
	RpcHandlers,
} from "#lib/types.ts";

afterEach(cleanup);

const databaseRequest: QuestionRequest = {
	id: "req-1",
	method: "item/tool/requestUserInput",
	threadId: "thread-1",
	turnId: "turn-1",
	itemId: "call-q",
	questions: [
		{
			id: "q1",
			header: "Database",
			question: "Which database should the service use?",
			isOther: false,
			multiSelect: false,
			options: [
				{
					label: "SQLite",
					description: "One file, no server to run.",
					preview: null,
				},
				{
					label: "Postgres",
					description: "A server with replication.",
					preview: "services:\n  db:\n    image: postgres:17",
				},
			],
		},
	],
};

const sandboxRequest: QuestionRequest = {
	id: "req-2",
	method: "item/tool/requestUserInput",
	threadId: "thread-1",
	turnId: "turn-1",
	itemId: "sandbox-1",
	questions: [
		{
			id: "sandbox",
			header: "Sandbox",
			question:
				"The harness sandbox is not available: Landlock is not available on this kernel. How should commands in this thread run?",
			isOther: false,
			multiSelect: false,
			options: [
				{
					label: "Accept",
					description:
						"Run without a sandbox. Every command asks for your approval first.",
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

function props(
	request: QuestionRequest,
	onSubmit: (
		request: QuestionRequest,
		selections: readonly QuestionSelection[],
	) => void,
	onDismiss: (request: QuestionRequest) => void,
) {
	return {
		request,
		position: 1,
		total: 1,
		subagent: null,
		onSubmit,
		onDismiss,
	};
}

function port(respond: (id: RequestId, result: JsonValue) => void): RpcPort {
	return {
		call: () => Promise.reject(new Error("no call in this test")),
		notify: (): void => undefined,
		respond,
		open: () => Promise.resolve(),
		close: (): void => undefined,
	};
}

describe("question card", () => {
	test("keeps the actions outside the scrolling details", () => {
		render(QuestionCard, {
			props: props(databaseRequest, vi.fn(), vi.fn()),
		});
		const details = screen.getByRole("region", { name: "Question details" });
		const submit = screen.getByRole("button", { name: "Submit" });
		expect(details.contains(submit)).toBe(false);
	});

	test("Escape dismisses with the cancel shape", async () => {
		const respond = vi.fn();
		let handlers: RpcHandlers | undefined;
		const session = new Session((_url, incoming) => {
			handlers = incoming;
			return port(respond);
		});
		session.start({
			hash: "#token=abc",
			host: "localhost:7437",
			pathname: "/",
			protocol: "http:",
			search: "",
		});
		if (handlers === undefined) throw new Error("the session did not dial");
		session.activeThreadId = "thread-1";
		handlers.onQuestion(databaseRequest);
		render(App, { props: { session } });
		await tick();
		const details = screen.getByRole("region", { name: "Question details" });
		await fireEvent.keyDown(details, { key: "Escape" });
		expect(respond).toHaveBeenCalledWith("req-1", { answers: {} });
	});

	test("Cancel on the sandbox choice sends {answers: {}}", async () => {
		const respond = vi.fn();
		let handlers: RpcHandlers | undefined;
		const session = new Session((_url, incoming) => {
			handlers = incoming;
			return port(respond);
		});
		session.start({
			hash: "#token=abc",
			host: "localhost:7437",
			pathname: "/",
			protocol: "http:",
			search: "",
		});
		if (handlers === undefined) throw new Error("the session did not dial");
		session.activeThreadId = "thread-1";
		handlers.onQuestion(sandboxRequest);
		render(App, { props: { session } });
		await tick();
		await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));
		expect(respond).toHaveBeenCalledWith("req-2", { answers: {} });
	});

	test("answers a question by keyboard alone", async () => {
		const onSubmit = vi.fn();
		const user = userEvent.setup();
		render(QuestionCard, {
			props: props(databaseRequest, onSubmit, vi.fn()),
		});
		await tick();
		await user.tab();
		await user.tab();
		await user.keyboard("{ArrowDown}");
		const submit = screen.getByRole("button", { name: "Submit" });
		for (let step = 0; step < 10 && document.activeElement !== submit; step++) {
			await user.tab();
		}
		await user.keyboard("{Enter}");
		expect(onSubmit).toHaveBeenCalledWith(databaseRequest, [
			{ picked: ["Postgres"], other: "" },
		]);
	});
});
