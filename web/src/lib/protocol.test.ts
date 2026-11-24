import { afterEach, describe, expect, it, vi } from "vitest";
import { RpcClient, rpcSocketUrl, tokenFromHash } from "./protocol.js";
import type { RpcHandlers } from "./types.js";

describe("tokenFromHash", () => {
	it("reads the token from the URL fragment", () => {
		expect(tokenFromHash("#token=abc123")).toBe("abc123");
	});

	it("returns an empty string when no token is present", () => {
		expect(tokenFromHash("")).toBe("");
		expect(tokenFromHash("#foo=bar")).toBe("");
	});
});

describe("rpcSocketUrl", () => {
	it("builds the websocket URL with an encoded token", () => {
		expect(rpcSocketUrl("example.com", true, "a b")).toBe(
			"wss://example.com/rpc?token=a%20b",
		);
		expect(rpcSocketUrl("example.com:8080", false, "abc")).toBe(
			"ws://example.com:8080/rpc?token=abc",
		);
		expect(rpcSocketUrl("example.com", true, "a&b=c+d/e")).toBe(
			"wss://example.com/rpc?token=a%26b%3Dc%2Bd%2Fe",
		);
	});
});

type Listener = (event: { readonly data?: unknown }) => void;

class FakeSocket {
	static readonly OPEN = 1;
	static opened: FakeSocket[] = [];
	readonly readyState = 1;
	readonly sent: string[] = [];
	private readonly listeners = new Map<string, Listener[]>();

	constructor() {
		FakeSocket.opened.push(this);
	}

	addEventListener(type: string, listener: Listener): void {
		this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
	}

	send(data: string): void {
		this.sent.push(data);
	}

	close(): void {}

	emit(type: string, data?: unknown): void {
		for (const listener of this.listeners.get(type) ?? []) listener({ data });
	}
}

function handlers(): RpcHandlers {
	return {
		onNotification: vi.fn(),
		onRequest: vi.fn(),
		onQuestion: vi.fn(),
		onQuestionWarning: vi.fn(),
		onStateChange: vi.fn(),
		onError: vi.fn(),
	};
}

async function connected(incoming: RpcHandlers): Promise<FakeSocket> {
	vi.stubGlobal("WebSocket", FakeSocket);
	const client = new RpcClient("ws://localhost/rpc", incoming);
	const opening = client.open();
	const socket = FakeSocket.opened.at(-1);
	if (socket === undefined) throw new Error("no socket opened");
	socket.emit("open");
	await opening;
	return socket;
}

describe("question warnings", () => {
	afterEach(() => {
		vi.unstubAllGlobals();
		FakeSocket.opened = [];
	});

	it("routes a question correction warning with its request id", async () => {
		const incoming = handlers();
		const socket = await connected(incoming);
		const params = {
			threadId: "thread-1",
			message: "invalid text: answers.q1.answers[1]: text_too_large",
			requestId: "req-1",
			reason: "text_too_large",
			field: "answers.q1.answers[1]",
		};
		socket.emit(
			"message",
			JSON.stringify({ jsonrpc: "2.0", method: "warning", params }),
		);
		expect(incoming.onQuestionWarning).toHaveBeenCalledExactlyOnceWith(params);
		expect(incoming.onError).not.toHaveBeenCalled();
	});

	it("leaves a plain warning without a request id to the notification path", async () => {
		const incoming = handlers();
		const socket = await connected(incoming);
		socket.emit(
			"message",
			JSON.stringify({
				jsonrpc: "2.0",
				method: "warning",
				params: { threadId: null, message: "disk almost full" },
			}),
		);
		expect(incoming.onQuestionWarning).not.toHaveBeenCalled();
		expect(incoming.onError).not.toHaveBeenCalled();
	});
});
