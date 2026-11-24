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

describe("question card answers", () => {
	afterEach(cleanup);

	const nameRequest: QuestionRequest = {
		id: "req-3",
		method: "item/tool/requestUserInput",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "call-name",
		questions: [
			{
				id: "name",
				header: "Name",
				question: "What should the service be called?",
				isOther: false,
				multiSelect: false,
				options: [],
			},
		],
	};

	function answerField(): HTMLTextAreaElement {
		return screen.getByRole("textbox", { name: "Name" }) as HTMLTextAreaElement;
	}

	test("an answer over 2000 characters stays with a field error and does not submit", async () => {
		const onSubmit = vi.fn();
		render(QuestionCard, { props: props(nameRequest, onSubmit, vi.fn()) });
		const over = `${"x".repeat(1999)}e\u0301`;
		await fireEvent.input(answerField(), { target: { value: over } });
		const alert = screen.getByRole("alert");
		expect(alert.textContent).toBe(
			"This answer has 2001 characters. Shorten it to 2000 or fewer.",
		);
		expect(answerField().getAttribute("aria-describedby")).toBe(alert.id);
		expect(answerField().getAttribute("aria-invalid")).toBe("true");
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).not.toHaveBeenCalled();
		expect(answerField().value).toBe(over);

		const emoji = "\u{1F600}".repeat(2000);
		await fireEvent.input(answerField(), { target: { value: emoji } });
		expect(screen.queryByRole("alert")).toBeNull();
		expect(answerField().getAttribute("aria-invalid")).toBe("false");
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).toHaveBeenCalledWith(nameRequest, [
			{ picked: [], other: emoji },
		]);
	});

	test("a pasted answer of 2000 emoji is not cut at 2000 UTF-16 units", async () => {
		const user = userEvent.setup();
		render(QuestionCard, { props: props(nameRequest, vi.fn(), vi.fn()) });
		const emoji = "\u{1F600}".repeat(2000);
		await user.click(answerField());
		await user.paste(emoji);
		expect(answerField().value).toBe(emoji);
	});

	test("an answer takes several lines and converts CRLF once", async () => {
		const onSubmit = vi.fn();
		render(QuestionCard, { props: props(nameRequest, onSubmit, vi.fn()) });
		await fireEvent.input(answerField(), {
			target: { value: "a\r\nb\rc\r\n" },
		});
		expect(answerField().value).toBe("a\nb\nc\n");
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).toHaveBeenCalledWith(nameRequest, [
			{ picked: [], other: "a\nb\nc\n" },
		]);
	});

	test("NUL in an answer is refused with a field error", async () => {
		const onSubmit = vi.fn();
		render(QuestionCard, { props: props(nameRequest, onSubmit, vi.fn()) });
		await fireEvent.input(answerField(), { target: { value: "a\0b" } });
		expect(screen.getByRole("alert").textContent).toBe(
			"This text has a NUL character. Remove it to continue.",
		);
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).not.toHaveBeenCalled();
	});

	test("Backspace in an answer selects the whole cluster first", async () => {
		render(QuestionCard, { props: props(nameRequest, vi.fn(), vi.fn()) });
		await fireEvent.input(answerField(), {
			target: { value: "\u1100\u1161\u11a8\u{20000}\u{E0100}" },
		});
		answerField().setSelectionRange(7, 7);
		await fireEvent.keyDown(answerField(), { key: "Backspace" });
		expect([answerField().selectionStart, answerField().selectionEnd]).toEqual([
			3, 7,
		]);
		answerField().setSelectionRange(3, 3);
		await fireEvent.keyDown(answerField(), { key: "Backspace" });
		expect([answerField().selectionStart, answerField().selectionEnd]).toEqual([
			0, 3,
		]);
	});

	test("the session sends padded and whitespace-only answers as typed", async () => {
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
		handlers.onQuestion(nameRequest);
		render(App, { props: { session } });
		await tick();
		await fireEvent.input(answerField(), { target: { value: " \u3000 " } });
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(respond).toHaveBeenCalledWith("req-3", {
			answers: { name: { answers: [" \u3000 "] } },
		});
	});

	test("model text shows control escapes and reveals invisible differences between options", async () => {
		const onSubmit = vi.fn();
		const twins: QuestionRequest = {
			...nameRequest,
			id: "req-4",
			questions: [
				{
					id: "pick",
					header: "Pick\u202e",
					question: "Which \x1b[1mone?",
					isOther: false,
					multiSelect: false,
					options: [
						{ label: "Yes", description: "plain\u0007", preview: "a\n\x1b[2J" },
						{ label: "Yes\u200b", description: "hidden", preview: null },
					],
				},
			],
		};
		render(QuestionCard, { props: props(twins, onSubmit, vi.fn()) });
		const group = screen.getByRole("group");
		const escapes = (selector: string): readonly (readonly string[])[] =>
			[...group.querySelectorAll(selector)].map((element) =>
				[...element.querySelectorAll(".escape")].map(
					(marker) => marker.getAttribute("data-escape") ?? "",
				),
			);
		const texts = (selector: string): readonly (string | null)[] =>
			[...group.querySelectorAll(selector)].map(
				(element) => element.textContent,
			);
		expect(texts("legend")).toEqual(["Pick\u202e"]);
		expect(escapes("legend")).toEqual([["U+202E"]]);
		expect(texts(".prompt")).toEqual(["Which \x1b[1mone?"]);
		expect(escapes(".prompt")).toEqual([["U+001B"]]);
		expect(texts(".choice")).toEqual(["Yes", "Yes\u200b"]);
		expect(escapes(".choice")).toEqual([[], ["U+200B"]]);
		expect(escapes(".description")).toEqual([["U+0007"], []]);
		const radios = screen.getAllByRole("radio");
		await fireEvent.click(radios[0] as HTMLElement);
		expect(texts(".preview")).toEqual(["a\n\x1b[2J"]);
		expect(escapes(".preview")).toEqual([["U+001B"]]);
		await fireEvent.click(radios[1] as HTMLElement);
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).toHaveBeenCalledWith(twins, [
			{ picked: ["Yes\u200b"], other: "" },
		]);
	});
});

describe("question answer correction", () => {
	afterEach(cleanup);

	const request: QuestionRequest = {
		id: "req-5",
		method: "item/tool/requestUserInput",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "call-name",
		questions: [
			{
				id: "name",
				header: "Name",
				question: "What should the service be called?",
				isOther: false,
				multiSelect: false,
				options: [],
			},
		],
	};

	const warning = {
		threadId: "thread-1",
		message: "invalid text: answers.name.answers[0]: disallowed_control",
		requestId: "req-5",
		reason: "disallowed_control",
		field: "answers.name.answers[0]",
	};

	function field(): HTMLTextAreaElement {
		return screen.getByRole("textbox", { name: "Name" }) as HTMLTextAreaElement;
	}

	function submit(): HTMLButtonElement {
		return screen.getByRole("button", { name: "Submit" }) as HTMLButtonElement;
	}

	function opened(respond: (id: RequestId, result: JsonValue) => void): {
		readonly session: Session;
		readonly handlers: RpcHandlers;
	} {
		let handlers: RpcHandlers | undefined;
		const session = new Session((_url, incoming) => {
			handlers = incoming;
			return {
				...port(respond),
				call: <T>(
					method: string,
					_params: JsonValue,
					parse: (value: unknown) => T,
				) =>
					method === "initialize"
						? Promise.resolve(
								parse({
									userAgent: "dalgona/0.1.0",
									platformFamily: "unix",
									platformOs: "linux",
								}),
							)
						: Promise.reject(new Error(`no ${method} in this test`)),
			};
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
		handlers.onQuestion(request);
		return { session, handlers };
	}

	test("the card waits for resolution, takes a correction and answers the same request again", async () => {
		const respond = vi.fn();
		const { session, handlers } = opened(respond);
		render(App, { props: { session } });
		await tick();
		await fireEvent.input(field(), { target: { value: "first draft" } });
		await fireEvent.click(submit());
		expect(respond).toHaveBeenCalledExactlyOnceWith("req-5", {
			answers: { name: { answers: ["first draft"] } },
		});
		expect(
			screen.getByRole("heading", { name: "Answer this question" }),
		).toBeTruthy();
		expect(
			screen.getByText("Waiting for dalgona to accept the answer."),
		).toBeTruthy();
		expect(field().readOnly).toBe(true);
		expect(submit().getAttribute("aria-disabled")).toBe("true");
		await fireEvent.click(submit());
		expect(respond).toHaveBeenCalledTimes(1);

		handlers.onQuestionWarning(warning);
		await tick();
		expect(field().readOnly).toBe(false);
		expect(field().value).toBe("first draft");
		const alert = document.getElementById(
			field().getAttribute("aria-describedby") ?? "",
		);
		expect(alert?.getAttribute("role")).toBe("alert");
		expect(alert?.textContent).toBe(
			"dalgona refused this answer because it has a NUL character. Remove it and submit again.",
		);

		await fireEvent.input(field(), { target: { value: "second draft" } });
		await fireEvent.click(submit());
		expect(respond).toHaveBeenCalledTimes(2);
		expect(respond).toHaveBeenLastCalledWith("req-5", {
			answers: { name: { answers: ["second draft"] } },
		});

		handlers.onNotification({
			type: "requestResolved",
			requestId: "req-5",
			resolution: "answered",
		});
		await tick();
		expect(
			screen.queryByRole("heading", { name: "Answer this question" }),
		).toBeNull();
		expect(document.body.textContent).not.toContain(
			"Question answered in another window",
		);
		handlers.onQuestionWarning(warning);
		await tick();
		expect(
			screen.queryByRole("heading", { name: "Answer this question" }),
		).toBeNull();
	});

	test("a warning for a request this window did not answer changes nothing", async () => {
		const respond = vi.fn();
		const { session, handlers } = opened(respond);
		render(App, { props: { session } });
		await tick();
		await fireEvent.input(field(), { target: { value: "draft" } });
		handlers.onQuestionWarning(warning);
		await tick();
		expect(field().getAttribute("aria-invalid")).toBe("false");
		expect(field().getAttribute("aria-describedby")).toBeNull();
	});
});

describe("question draft retention", () => {
	afterEach(cleanup);

	const request: QuestionRequest = {
		id: "req-9",
		method: "item/tool/requestUserInput",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "call-name",
		questions: [
			{
				id: "name",
				header: "Name",
				question: "What should the service be called?",
				isOther: false,
				multiSelect: false,
				options: [],
			},
		],
	};

	const warning = {
		threadId: "thread-1",
		message: "invalid text: answers.name.answers[0]: disallowed_control",
		requestId: "req-9",
		reason: "disallowed_control",
		field: "answers.name.answers[0]",
	};

	function field(): HTMLTextAreaElement {
		return screen.getByRole("textbox", { name: "Name" }) as HTMLTextAreaElement;
	}

	function submit(): HTMLButtonElement {
		return screen.getByRole("button", { name: "Submit" }) as HTMLButtonElement;
	}

	function opened(respond: (id: RequestId, result: JsonValue) => void): {
		readonly session: Session;
		readonly handlers: RpcHandlers;
	} {
		let handlers: RpcHandlers | undefined;
		const session = new Session((_url, incoming) => {
			handlers = incoming;
			return {
				...port(respond),
				call: <T>(
					method: string,
					_params: JsonValue,
					parse: (value: unknown) => T,
				) =>
					method === "initialize"
						? Promise.resolve(
								parse({
									userAgent: "dalgona/0.1.0",
									platformFamily: "unix",
									platformOs: "linux",
								}),
							)
						: Promise.reject(new Error(`no ${method} in this test`)),
			};
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
		handlers.onQuestion(request);
		return { session, handlers };
	}

	test("a remount after submitting keeps the answers until the request resolves", async () => {
		const respond = vi.fn();
		const { session, handlers } = opened(respond);
		const view = render(App, { props: { session } });
		await tick();
		await fireEvent.input(field(), { target: { value: "first draft" } });
		await fireEvent.click(submit());
		expect(respond).toHaveBeenCalledExactlyOnceWith("req-9", {
			answers: { name: { answers: ["first draft"] } },
		});
		view.unmount();
		render(App, { props: { session } });
		await tick();
		expect(field().value).toBe("first draft");
		expect(field().readOnly).toBe(true);
		handlers.onQuestionWarning(warning);
		await tick();
		expect(field().readOnly).toBe(false);
		expect(field().value).toBe("first draft");
	});
});

describe("question other atomicity", () => {
	afterEach(cleanup);

	const otherRequest: QuestionRequest = {
		id: "req-other",
		method: "item/tool/requestUserInput",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "call-pick",
		questions: [
			{
				id: "pick",
				header: "Pick",
				question: "Choose one.",
				isOther: true,
				multiSelect: false,
				options: [{ label: "Alpha", description: "First.", preview: null }],
			},
		],
	};

	function otherField(): HTMLTextAreaElement {
		return screen.getByRole("textbox", {
			name: "Other answer for Pick",
		}) as HTMLTextAreaElement;
	}

	function alpha(): HTMLInputElement {
		return screen.getByRole("radio", { name: /Alpha/ }) as HTMLInputElement;
	}

	test("a rejected Other candidate keeps the selected option", async () => {
		const onSubmit = vi.fn();
		render(QuestionCard, { props: props(otherRequest, onSubmit, vi.fn()) });
		await fireEvent.click(alpha());
		expect(alpha().checked).toBe(true);
		const over = "x".repeat(2001);
		await fireEvent.input(otherField(), { target: { value: over } });
		expect(screen.getByRole("alert").textContent).toBe(
			"This answer has 2001 characters. Shorten it to 2000 or fewer.",
		);
		expect(otherField().value).toBe(over);
		expect(alpha().checked).toBe(true);
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).not.toHaveBeenCalled();
		await fireEvent.input(otherField(), { target: { value: "ok" } });
		expect(alpha().checked).toBe(false);
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).toHaveBeenCalledWith(otherRequest, [
			{ picked: [], other: "ok" },
		]);
	});

	test("a composing Other input does not replace the committed selection", async () => {
		const onSubmit = vi.fn();
		render(QuestionCard, { props: props(otherRequest, onSubmit, vi.fn()) });
		await fireEvent.click(alpha());
		const field = otherField();
		await fireEvent.input(field, {
			target: { value: "あ" },
			isComposing: true,
		});
		await tick();
		expect(alpha().checked).toBe(true);
		expect(field.value).toBe("あ");
		expect(screen.queryByRole("alert")).toBeNull();
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).toHaveBeenCalledWith(otherRequest, [
			{ picked: ["Alpha"], other: "" },
		]);
	});
});

describe("question whitespace twins", () => {
	afterEach(cleanup);

	test("options that collide after whitespace rendering reveal their code points", async () => {
		const onSubmit = vi.fn();
		const twins: QuestionRequest = {
			id: "req-ws",
			method: "item/tool/requestUserInput",
			threadId: "thread-1",
			turnId: "turn-1",
			itemId: "call-ws",
			questions: [
				{
					id: "pick",
					header: "Pick",
					question: "Choose one.",
					isOther: false,
					multiSelect: false,
					options: [
						{ label: "Read only", description: "First.", preview: null },
						{ label: "Read  only", description: "Second.", preview: null },
					],
				},
			],
		};
		render(QuestionCard, { props: props(twins, onSubmit, vi.fn()) });
		const group = screen.getByRole("group");
		const choices = [...group.querySelectorAll(".choice")];
		expect(choices).toHaveLength(2);
		const escapes = choices.map((choice) =>
			[...choice.querySelectorAll(".escape")].map(
				(marker) => marker.getAttribute("data-escape") ?? "",
			),
		);
		expect(escapes).toEqual([["U+0020"], ["U+0020", "U+0020"]]);
		await fireEvent.click(screen.getAllByRole("radio")[1] as HTMLElement);
		await fireEvent.click(screen.getByRole("button", { name: "Submit" }));
		expect(onSubmit).toHaveBeenCalledWith(twins, [
			{ picked: ["Read  only"], other: "" },
		]);
	});
});
