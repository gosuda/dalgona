import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { tick } from "svelte";
import { afterEach, describe, expect, type Mock, test, vi } from "vitest";
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

function mockScrolling(details: HTMLElement) {
	const scrollBy = vi.fn();
	const scrollTo = vi.fn();
	const metrics = { scrollBy, scrollTo, scrollHeight: 1000, clientHeight: 200 };
	for (const [name, value] of Object.entries(metrics)) {
		Object.defineProperty(details, name, { value, configurable: true });
	}
	return { scrollBy, scrollTo };
}

function lastTop(scroll: Mock): number {
	const options = scroll.mock.lastCall?.[0] as { top: number } | undefined;
	if (options === undefined) throw new Error("the details did not scroll");
	return options.top;
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

	test("scrolls the details from the title and the actions with the keyboard", async () => {
		render(QuestionCard, {
			props: props(databaseRequest, vi.fn(), vi.fn()),
		});
		const { scrollBy, scrollTo } = mockScrolling(
			screen.getByRole("region", { name: "Question details" }),
		);
		const title = screen.getByRole("heading", { name: "Answer this question" });
		const submit = screen.getByRole("button", { name: "Submit" });
		const dismiss = screen.getByRole("button", { name: "Dismiss" });

		expect(await fireEvent.keyDown(title, { key: "ArrowDown" })).toBe(false);
		const line = lastTop(scrollBy);
		expect(line).toBeGreaterThan(0);
		expect(await fireEvent.keyDown(title, { key: "ArrowUp" })).toBe(false);
		expect(lastTop(scrollBy)).toBe(-line);

		expect(await fireEvent.keyDown(submit, { key: "PageDown" })).toBe(false);
		const page = lastTop(scrollBy);
		expect(page).toBeGreaterThan(line);
		expect(page).toBeLessThanOrEqual(200);
		expect(await fireEvent.keyDown(dismiss, { key: "PageUp" })).toBe(false);
		expect(lastTop(scrollBy)).toBe(-page);

		expect(await fireEvent.keyDown(title, { key: "Home" })).toBe(false);
		expect(lastTop(scrollTo)).toBe(0);
		expect(await fireEvent.keyDown(dismiss, { key: "End" })).toBe(false);
		expect(lastTop(scrollTo)).toBe(1000);
	});

	test("leaves scrolling keys to a radio and a text field in the details", async () => {
		const [first] = databaseRequest.questions;
		if (first === undefined) throw new Error("no question in the fixture");
		render(QuestionCard, {
			props: props(
				{ ...databaseRequest, questions: [{ ...first, isOther: true }] },
				vi.fn(),
				vi.fn(),
			),
		});
		const { scrollBy, scrollTo } = mockScrolling(
			screen.getByRole("region", { name: "Question details" }),
		);
		const radio = screen.getByRole("radio", { name: /SQLite/ });
		const field = screen.getByRole("textbox", {
			name: "Other answer for Database",
		});
		const title = screen.getByRole("heading", { name: "Answer this question" });

		expect(await fireEvent.keyDown(radio, { key: "ArrowDown" })).toBe(true);
		expect(await fireEvent.keyDown(field, { key: "Home" })).toBe(true);
		expect(await fireEvent.keyDown(field, { key: "PageDown" })).toBe(true);
		expect(
			await fireEvent.keyDown(title, { key: "PageDown", metaKey: true }),
		).toBe(true);
		expect(scrollBy).not.toHaveBeenCalled();
		expect(scrollTo).not.toHaveBeenCalled();
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
		expect(document.activeElement).toBe(
			screen.getByRole("radio", { name: /SQLite/ }),
		);
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
