import {
	cleanup,
	fireEvent,
	render,
	screen,
	within,
} from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { tick } from "svelte";
import { afterEach, describe, expect, type Mock, test, vi } from "vitest";
import App from "#lib/components/App.svelte";
import QuestionCard from "#lib/components/QuestionCard.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type {
	AppNotification,
	JsonValue,
	QuestionRequest,
	QuestionSelection,
	RequestId,
	RpcHandlers,
} from "#lib/types.ts";
import { parseNotification } from "#lib/validation.ts";

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
		submission: null,
		onSubmit,
		onDismiss,
	};
}

const otherRequest: QuestionRequest = {
	...databaseRequest,
	questions: databaseRequest.questions.map((question) => ({
		...question,
		isOther: true,
	})),
};

function notice(method: string, params: JsonValue): AppNotification {
	const parsed = parseNotification({ method, params });
	if (parsed === null) throw new Error(`${method} did not parse`);
	return parsed;
}

function correction(field: string, reason: string): AppNotification {
	return notice("warning", {
		threadId: "thread-1",
		requestId: "req-1",
		message: `invalid text: ${field}: ${reason}`,
		reason,
		field,
	});
}

function openApp(respond: (id: RequestId, result: JsonValue) => void): {
	handlers: RpcHandlers;
	session: Session;
} {
	let handlers: RpcHandlers | undefined;
	const session = new Session((_url, incoming) => {
		handlers = incoming;
		return { ...port(respond), call: () => new Promise<never>(() => {}) };
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
	return { handlers, session };
}

function pasted(text: string) {
	return { getData: (type: string) => (type === "text/plain" ? text : "") };
}

function otherField(): HTMLInputElement {
	return screen.getByRole("textbox", {
		name: "Other answer for Database",
	}) as HTMLInputElement;
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
		expect(lastTop(scrollBy)).toBe(48);
		expect(await fireEvent.keyDown(title, { key: "ArrowUp" })).toBe(false);
		expect(lastTop(scrollBy)).toBe(-48);

		expect(await fireEvent.keyDown(submit, { key: "PageDown" })).toBe(false);
		expect(lastTop(scrollBy)).toBe(152);
		expect(await fireEvent.keyDown(dismiss, { key: "PageUp" })).toBe(false);
		expect(lastTop(scrollBy)).toBe(-152);

		expect(await fireEvent.keyDown(title, { key: "Home" })).toBe(false);
		expect(lastTop(scrollTo)).toBe(0);
		expect(await fireEvent.keyDown(dismiss, { key: "End" })).toBe(false);
		expect(lastTop(scrollTo)).toBe(1000);
	});

	test("names the question as the group description", () => {
		render(QuestionCard, {
			props: props(databaseRequest, vi.fn(), vi.fn()),
		});
		const group = screen.getByRole("group", {
			name: "Database",
			description: "Which database should the service use?",
		});
		const prompt = group.querySelector<HTMLElement>("p.prompt");
		expect(prompt?.id).toBe(group.getAttribute("aria-describedby"));
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

describe("question display", () => {
	test("shows look-alike labels apart and submits the offered label", async () => {
		const onSubmit = vi.fn();
		const request: QuestionRequest = {
			...databaseRequest,
			questions: [
				{
					id: "q1",
					header: "Pick\u202e",
					question: "Which\none?",
					isOther: false,
					multiSelect: false,
					options: [
						{ label: "Yes", description: "a\tb", preview: null },
						{ label: "Yes\u200b", description: "", preview: "\n\tcode\u0000" },
					],
				},
			],
		};
		render(QuestionCard, { props: props(request, onSubmit, vi.fn()) });
		expect(screen.getByText("Pick<U+202E>")).toBeTruthy();
		expect(screen.getByText("Which one?")).toBeTruthy();
		expect(screen.getByText("a b")).toBeTruthy();
		expect(screen.getByText("Yes")).toBeTruthy();
		await fireEvent.click(screen.getByRole("radio", { name: /Yes<U\+200B>/ }));
		expect(document.querySelector(".preview")?.textContent).toBe(
			"    code<U+0000>",
		);
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).toHaveBeenCalledWith(request, [
			{ picked: ["Yes\u200b"], other: "" },
		]);
	});
});

describe("question answer text", () => {
	test("keeps an over-limit answer in the field with an error and does not submit it", async () => {
		const onSubmit = vi.fn();
		render(QuestionCard, { props: props(otherRequest, onSubmit, vi.fn()) });
		expect(otherField().hasAttribute("maxlength")).toBe(false);
		const full = "\u{1F600}".repeat(2000);
		await fireEvent.input(otherField(), { target: { value: `${full}x` } });
		expect(otherField().value).toBe(`${full}x`);
		const alert = screen.getByRole("alert");
		expect(alert.textContent).toBe(
			"Shorten this answer to 2000 characters or fewer.",
		);
		expect(otherField().getAttribute("aria-describedby")).toBe(alert.id);
		expect(otherField().getAttribute("aria-invalid")).toBe("true");
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).not.toHaveBeenCalled();
		await fireEvent.input(otherField(), { target: { value: full } });
		expect(screen.queryByRole("alert")).toBeNull();
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).toHaveBeenCalledWith(otherRequest, [
			{ picked: [], other: full },
		]);
	});

	test("refuses a multiline paste whole and keeps the answer", async () => {
		render(QuestionCard, { props: props(otherRequest, vi.fn(), vi.fn()) });
		await fireEvent.input(otherField(), { target: { value: "keep" } });
		expect(
			await fireEvent.paste(otherField(), {
				clipboardData: pasted("a\u2028b"),
			}),
		).toBe(false);
		expect(otherField().value).toBe("keep");
		expect(screen.getByRole("alert").textContent).toBe(
			"This field takes one line. Nothing was pasted.",
		);
		expect(
			await fireEvent.paste(otherField(), {
				clipboardData: pasted("one line"),
			}),
		).toBe(true);
		await fireEvent.input(otherField(), { target: { value: "keep it" } });
		expect(screen.queryByRole("alert")).toBeNull();
	});

	test("refuses a NUL answer and submits whitespace as typed", async () => {
		const onSubmit = vi.fn();
		render(QuestionCard, { props: props(otherRequest, onSubmit, vi.fn()) });
		await fireEvent.input(otherField(), { target: { value: "a\u0000b" } });
		expect(screen.getByRole("alert").textContent).toBe(
			"Remove the NUL character from this answer.",
		);
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).not.toHaveBeenCalled();
		await fireEvent.input(otherField(), { target: { value: " \u3000 " } });
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).toHaveBeenCalledWith(otherRequest, [
			{ picked: [], other: " \u3000 " },
		]);
	});

	test("sends nothing while a submitted answer waits", async () => {
		const onSubmit = vi.fn();
		const onDismiss = vi.fn();
		render(QuestionCard, {
			props: {
				...props(otherRequest, onSubmit, onDismiss),
				submission: {
					selections: [{ picked: [], other: "sent" }],
					correction: null,
				},
			},
		});
		expect(otherField().value).toBe("sent");
		for (const name of ["Submit", "Skip", "Dismiss"]) {
			const button = screen.getByRole("button", { name });
			expect(button.getAttribute("aria-disabled")).toBe("true");
			await fireEvent.click(button);
		}
		await fireEvent.keyDown(screen.getByRole("heading"), { key: "Escape" });
		expect(onSubmit).not.toHaveBeenCalled();
		expect(onDismiss).not.toHaveBeenCalled();
		expect(screen.getByRole("status").textContent).toBe(
			"Answer sent. Waiting for the question to close.",
		);
	});

	test("restores submitted answers and shows a correction it cannot place", () => {
		render(QuestionCard, {
			props: {
				...props(databaseRequest, vi.fn(), vi.fn()),
				submission: {
					selections: [{ picked: ["Postgres"], other: "" }],
					correction: {
						field: "answers.gone.answers[0]",
						reason: "text_too_large",
						message: "invalid text",
					},
				},
			},
		});
		expect(
			(screen.getByRole("radio", { name: /Postgres/ }) as HTMLInputElement)
				.checked,
		).toBe(true);
		expect(screen.getByRole("alert").textContent).toBe(
			"Shorten this answer to 2000 characters or fewer.",
		);
	});

	test("keeps the card after Submit until the request resolves and resubmits after a warning", async () => {
		const respond = vi.fn();
		const { handlers, session } = openApp(respond);
		handlers.onQuestion(otherRequest);
		render(App, { props: { session } });
		await tick();
		await fireEvent.input(otherField(), { target: { value: " two words " } });
		const card = within(
			screen.getByRole("region", { name: "Answer this question" }),
		);
		const submit = card.getByRole("button", { name: "Submit" });
		submit.focus();
		await fireEvent.click(submit);
		expect(respond).toHaveBeenCalledWith("req-1", {
			answers: { q1: { answers: [" two words "] } },
		});
		expect(
			screen.getByText("Answer sent. Waiting for the question to close."),
		).toBeTruthy();
		expect(submit.getAttribute("aria-disabled")).toBe("true");
		await fireEvent.click(submit);
		await fireEvent.click(card.getByRole("button", { name: "Skip" }));
		await fireEvent.click(card.getByRole("button", { name: "Dismiss" }));
		await fireEvent.keyDown(submit, { key: "Escape" });
		expect(respond).toHaveBeenCalledTimes(1);

		handlers.onNotification(
			correction("answers.q1.answers[0]", "disallowed_control"),
		);
		await tick();
		expect(otherField().value).toBe(" two words ");
		const error = screen.getByText(
			"Remove the NUL character from this answer.",
		);
		expect(otherField().getAttribute("aria-describedby")).toBe(error.id);
		expect(submit.getAttribute("aria-disabled")).toBeNull();
		expect(document.activeElement).toBe(submit);
		await fireEvent.click(submit);
		expect(respond).toHaveBeenCalledTimes(1);

		await fireEvent.input(otherField(), {
			target: { value: " two words, fixed " },
		});
		expect(
			screen.queryByText("Remove the NUL character from this answer."),
		).toBeNull();
		await fireEvent.click(submit);
		expect(respond).toHaveBeenCalledTimes(2);
		expect(respond).toHaveBeenLastCalledWith("req-1", {
			answers: { q1: { answers: [" two words, fixed "] } },
		});

		handlers.onNotification(
			notice("serverRequest/resolved", {
				threadId: "thread-1",
				requestId: "req-1",
				resolution: "answered",
			}),
		);
		await tick();
		expect(
			screen.queryByRole("region", { name: "Question details" }),
		).toBeNull();
		expect(screen.getByText("Question answered")).toBeTruthy();
		handlers.onNotification(
			correction("answers.q1.answers[0]", "disallowed_control"),
		);
		await tick();
		expect(
			screen.queryByRole("region", { name: "Question details" }),
		).toBeNull();
		expect(respond).toHaveBeenCalledTimes(2);
	});
});
