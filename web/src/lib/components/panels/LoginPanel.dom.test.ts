import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import LoginPanel from "#lib/components/panels/LoginPanel.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type {
	AppNotification,
	ApprovalRequest,
	JsonValue,
	RequestId,
	RpcHandlers,
} from "#lib/types.ts";

afterEach(cleanup);

type Answer = (params: JsonValue) => JsonValue;

interface Call {
	readonly method: string;
	readonly params: JsonValue;
}

const location = {
	hash: "#token=abc",
	host: "localhost:7437",
	pathname: "/",
	protocol: "http:",
	search: "",
};

class FakeRpc implements RpcPort {
	private readonly log: Call[] = [];

	get calls(): readonly Call[] {
		return this.log;
	}
	private readonly handlers: RpcHandlers;
	private readonly table: Record<string, Answer>;

	constructor(handlers: RpcHandlers, table: Record<string, Answer>) {
		this.handlers = handlers;
		this.table = table;
	}

	open(): Promise<void> {
		this.handlers.onStateChange("connected");
		return Promise.resolve();
	}

	call<T>(
		method: string,
		params: JsonValue,
		parse: (value: unknown) => T,
	): Promise<T> {
		this.log.push({ method, params });
		const answer = this.table[method];
		if (answer === undefined) {
			return Promise.reject(new Error(`no answer for ${method}`));
		}
		try {
			return Promise.resolve(parse(answer(params)));
		} catch (error) {
			return Promise.reject(error);
		}
	}

	notify(_method: string, _params: JsonValue): void {}

	deliver(notification: AppNotification): void {
		this.handlers.onNotification(notification);
	}

	request(request: ApprovalRequest): void {
		this.handlers.onRequest(request);
	}

	respond(_id: RequestId, _result: JsonValue): void {}

	close(): void {}
}

const baseAnswers: Record<string, Answer> = {
	initialize: () => ({ userAgent: "dalgona/0.1.0", platformOs: "linux" }),
	"thread/list": () => ({ data: [], nextCursor: null }),
	"model/list": () => ({ data: [] }),
	"config/read": () => ({ config: { model: "gpt-test" }, origins: {} }),
	"dalgona/keywords/list": () => [],
	"dalgona/settings/read": () => ({ settings: [] }),
};

async function connected(
	table: Record<string, Answer>,
): Promise<{ session: Session; rpc: FakeRpc }> {
	let rpc: FakeRpc | undefined;
	const session = new Session(
		(_url, handlers) => {
			rpc = new FakeRpc(handlers, { ...baseAnswers, ...table });
			return rpc;
		},
		() => {},
	);
	session.start(location);
	await vi.waitFor(() => expect(session.connection).toBe("connected"));
	if (rpc === undefined) throw new Error("no rpc port");
	return { session, rpc };
}

function providers(
	list: readonly { id: string; login: string; signedIn: boolean }[],
): Answer {
	return () => ({ providers: list });
}

describe("login panel", () => {
	test("an API key login sends provider and key", async () => {
		const { session, rpc } = await connected({
			"dalgona/account/providers/list": providers([
				{ id: "openai", login: "apiKey", signedIn: false },
			]),
			"account/login/start": () => ({ type: "apiKey" }),
		});
		session.openPanel("login");
		render(LoginPanel, {
			props: { session, open: true, onClose: vi.fn() },
		});
		await fireEvent.click(await screen.findByLabelText(/openai/));
		await fireEvent.input(screen.getByLabelText("API key for openai"), {
			target: { value: "sk-test" },
		});
		await fireEvent.click(screen.getByRole("button", { name: "Log in" }));
		await vi.waitFor(() =>
			expect(rpc.calls).toContainEqual({
				method: "account/login/start",
				params: { type: "apiKey", provider: "openai", apiKey: "sk-test" },
			}),
		);
	});

	test("a browser login waits for account/login/completed and reloads models", async () => {
		const { session, rpc } = await connected({
			"dalgona/account/providers/list": providers([
				{ id: "chatgpt", login: "browser", signedIn: false },
			]),
			"account/login/start": () => ({
				type: "chatgpt",
				loginId: "login-1",
				authUrl: "https://auth.example/start",
			}),
		});
		session.openPanel("login");
		render(LoginPanel, {
			props: { session, open: true, onClose: vi.fn() },
		});
		await fireEvent.click(await screen.findByLabelText(/chatgpt/));
		await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
		const link = await screen.findByRole("link", {
			name: "Open sign-in page",
		});
		expect(link.getAttribute("href")).toBe("https://auth.example/start");
		expect(
			rpc.calls.filter((call) => call.method === "model/list"),
		).toHaveLength(1);
		rpc.deliver({
			type: "loginCompleted",
			loginId: "login-1",
			success: true,
			error: null,
			provider: "chatgpt",
		});
		await vi.waitFor(() =>
			expect(
				rpc.calls.filter((call) => call.method === "model/list"),
			).toHaveLength(2),
		);
		expect(session.panel).toBeNull();
		expect(session.notices.map((notice) => notice.text)).toContain(
			"Signed in to chatgpt",
		);
	});

	test("a failed login keeps the panel open with Try again", async () => {
		const { session } = await connected({
			"dalgona/account/providers/list": providers([
				{ id: "chatgpt", login: "browser", signedIn: false },
			]),
			"account/login/start": () => {
				throw new Error("chatgpt login is not available");
			},
		});
		session.openPanel("login");
		render(LoginPanel, {
			props: { session, open: true, onClose: vi.fn() },
		});
		await fireEvent.click(await screen.findByLabelText(/chatgpt/));
		await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
		await vi.waitFor(() =>
			expect(
				session.notices.find((notice) => notice.tone === "error")?.text,
			).toBe("Login failed: chatgpt login is not available; run /login again"),
		);
		const failure = session.notices.find((notice) => notice.tone === "error");
		expect(failure?.actions.map((action) => action.label)).toEqual([
			"Try again",
		]);
		expect(session.panel).toBe("login");
		expect(screen.getByRole("button", { name: "Continue" })).toBeTruthy();
	});

	test("a failed API key login keeps the field editable and shows the error under it", async () => {
		let attempt = 0;
		const { session, rpc } = await connected({
			"dalgona/account/providers/list": providers([
				{ id: "openai", login: "apiKey", signedIn: false },
			]),
			"account/login/start": () => {
				attempt += 1;
				if (attempt === 1) throw new Error("openai did not accept the API key");
				return { type: "apiKey" };
			},
		});
		session.openPanel("login");
		render(LoginPanel, {
			props: { session, open: true, onClose: vi.fn() },
		});
		await fireEvent.click(await screen.findByLabelText(/openai/));
		const field = screen.getByLabelText(
			"API key for openai",
		) as HTMLInputElement;
		await fireEvent.input(field, { target: { value: "sk-wrong" } });
		await fireEvent.click(screen.getByRole("button", { name: "Log in" }));
		await screen.findByText("openai did not accept the API key");
		expect(field.disabled).toBe(false);
		expect(
			(screen.getByRole("button", { name: "Log in" }) as HTMLButtonElement)
				.disabled,
		).toBe(false);
		const errorId = field.getAttribute("aria-describedby") ?? "";
		expect(document.getElementById(errorId)?.textContent).toBe(
			"openai did not accept the API key",
		);
		await fireEvent.input(field, { target: { value: "sk-good" } });
		await fireEvent.click(screen.getByRole("button", { name: "Log in" }));
		await vi.waitFor(() =>
			expect(rpc.calls).toContainEqual({
				method: "account/login/start",
				params: { type: "apiKey", provider: "openai", apiKey: "sk-good" },
			}),
		);
	});

	test("an API key login closes when the server confirms it", async () => {
		const { session, rpc } = await connected({
			"dalgona/account/providers/list": providers([
				{ id: "openai", login: "apiKey", signedIn: false },
			]),
			"account/login/start": () => ({ type: "apiKey" }),
		});
		session.openPanel("login");
		render(LoginPanel, {
			props: { session, open: true, onClose: vi.fn() },
		});
		await fireEvent.click(await screen.findByLabelText(/openai/));
		await fireEvent.input(screen.getByLabelText("API key for openai"), {
			target: { value: "sk-test" },
		});
		await fireEvent.click(screen.getByRole("button", { name: "Log in" }));
		await vi.waitFor(() =>
			expect(
				rpc.calls.some((call) => call.method === "account/login/start"),
			).toBe(true),
		);
		rpc.deliver({
			type: "loginCompleted",
			loginId: null,
			success: true,
			error: null,
			provider: "openai",
		});
		expect(session.panel).toBeNull();
		expect(session.notices.map((notice) => notice.text)).toContain(
			"Signed in to openai",
		);
	});

	test("closing the login panel cancels a pending browser login once", async () => {
		const { session, rpc } = await connected({
			"dalgona/account/providers/list": providers([
				{ id: "chatgpt", login: "browser", signedIn: false },
			]),
			"account/login/start": () => ({
				type: "chatgpt",
				loginId: "login-1",
				authUrl: "https://auth.example/start",
			}),
		});
		session.openPanel("login");
		await session.startLogin("chatgpt", "browser");
		session.closePanel();
		expect(cancelCalls(rpc)).toEqual([
			{ method: "account/login/cancel", params: { loginId: "login-1" } },
		]);
	});

	test("a settings save leaves a pending browser login alone", async () => {
		const { session, rpc } = await connected({
			"account/login/start": () => ({
				type: "chatgpt",
				loginId: "login-1",
				authUrl: "https://auth.example/start",
			}),
			"config/batchWrite": () => ({
				filePath: "/home/settings.toml",
				status: "ok",
				version: "v2",
			}),
		});
		session.openPanel("settings");
		await session.startLogin("chatgpt", "browser");
		await session.saveSettings([{ keyPath: "verbosity", value: "high" }]);
		expect(cancelCalls(rpc)).toEqual([]);
	});

	test("opening another panel cancels a pending browser login", async () => {
		const { session, rpc } = await connected({
			"dalgona/account/providers/list": providers([
				{ id: "chatgpt", login: "browser", signedIn: false },
			]),
			"account/login/start": () => ({
				type: "chatgpt",
				loginId: "login-1",
				authUrl: "https://auth.example/start",
			}),
		});
		session.openPanel("login");
		await session.startLogin("chatgpt", "browser");
		session.openPanel("settings");
		expect(cancelCalls(rpc)).toEqual([
			{ method: "account/login/cancel", params: { loginId: "login-1" } },
		]);
	});

	test("a start response that arrives after the user cancelled is cancelled on the server", async () => {
		const { session, rpc } = await connected({
			"dalgona/account/providers/list": providers([
				{ id: "chatgpt", login: "browser", signedIn: false },
			]),
			"account/login/start": () => ({
				type: "chatgpt",
				loginId: "login-late",
				authUrl: "https://auth.example/start",
			}),
		});
		const { promise: gate, resolve: release } = Promise.withResolvers<void>();
		const call = rpc.call.bind(rpc);
		rpc.call = <T>(
			method: string,
			params: JsonValue,
			parse: (value: unknown) => T,
		): Promise<T> =>
			method === "account/login/start"
				? gate.then(() => call(method, params, parse))
				: call(method, params, parse);
		session.openPanel("login");
		const pending = session.startLogin("chatgpt", "browser");
		session.cancelLogin();
		release();
		await pending;
		expect(cancelCalls(rpc)).toEqual([
			{ method: "account/login/cancel", params: { loginId: "login-late" } },
		]);
	});

	test("a failed browser login does not reload models", async () => {
		const { session, rpc } = await connected({
			"account/login/start": () => ({
				type: "chatgpt",
				loginId: "login-1",
				authUrl: "https://auth.example/start",
			}),
		});
		await session.startLogin("chatgpt", "browser");
		rpc.deliver({
			type: "loginCompleted",
			loginId: "login-1",
			success: false,
			error: "denied",
			provider: "chatgpt",
		});
		expect(modelCalls(rpc)).toHaveLength(1);
	});

	test("another flow's completion does not reload models or end this flow", async () => {
		const { session, rpc } = await connected({
			"account/login/start": () => ({
				type: "chatgpt",
				loginId: "login-1",
				authUrl: "https://auth.example/start",
			}),
		});
		await session.startLogin("chatgpt", "browser");
		rpc.deliver({
			type: "loginCompleted",
			loginId: "login-other",
			success: true,
			error: null,
			provider: "chatgpt",
		});
		expect(modelCalls(rpc)).toHaveLength(1);
		expect(session.login?.loginId).toBe("login-1");
	});
});

function cancelCalls(rpc: FakeRpc): readonly Call[] {
	return rpc.calls.filter((call) => call.method === "account/login/cancel");
}

function modelCalls(rpc: FakeRpc): readonly Call[] {
	return rpc.calls.filter((call) => call.method === "model/list");
}
