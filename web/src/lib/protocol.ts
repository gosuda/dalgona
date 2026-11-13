import { parseQuestionRequest } from "./question";
import type {
	ApprovalRequest,
	JsonValue,
	RequestId,
	RpcHandlers,
} from "./types";
import {
	parseApprovalRequest,
	parseJsonValue,
	parseNotification,
} from "./validation";

interface PendingCall {
	readonly accept: (value: unknown) => void;
	readonly reject: (error: Error) => void;
}

const questionMethod = "item/tool/requestUserInput";

interface RpcFields {
	readonly jsonrpc: JsonValue | undefined;
	readonly id: JsonValue | undefined;
	readonly method: JsonValue | undefined;
	readonly params: JsonValue | undefined;
	readonly result: JsonValue | undefined;
	readonly error: JsonValue | undefined;
	readonly hasResult: boolean;
	readonly hasError: boolean;
}

function readField(value: object, key: string): JsonValue | undefined {
	return parseJsonValue(Reflect.get(value, key));
}

function rpcFields(value: unknown): RpcFields | null {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		return null;
	}
	return {
		jsonrpc: readField(value, "jsonrpc"),
		id: readField(value, "id"),
		method: readField(value, "method"),
		params: readField(value, "params"),
		result: readField(value, "result"),
		error: readField(value, "error"),
		hasResult: "result" in value,
		hasError: "error" in value,
	};
}

export class RpcError extends Error {
	readonly code: number | null;

	constructor(code: number | null, message: string) {
		super(`rpc error ${code ?? "?"}: ${message}`);
		this.name = "RpcError";
		this.code = code;
	}
}

function rpcError(value: JsonValue | undefined): RpcError {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		return new RpcError(null, "rpc error");
	}
	const code = readField(value, "code");
	const message = readField(value, "message");
	return new RpcError(
		typeof code === "number" ? code : null,
		typeof message === "string" ? message : "rpc error",
	);
}

export function tokenFromHash(hash: string): string {
	const params = new URLSearchParams(
		hash.startsWith("#") ? hash.slice(1) : hash,
	);
	return params.get("token") ?? "";
}

export function rpcSocketUrl(
	host: string,
	secure: boolean,
	token: string,
): string {
	const scheme = secure ? "wss" : "ws";
	return `${scheme}://${host}/rpc?token=${encodeURIComponent(token)}`;
}

export class RpcClient {
	private readonly url: string;
	private readonly handlers: RpcHandlers;
	private socket: WebSocket | null = null;
	private nextId = 1;
	private readonly pending = new Map<number, PendingCall>();
	private opened: Promise<void> | null = null;
	private failOpen: ((error: Error) => void) | null = null;

	constructor(url: string, handlers: RpcHandlers) {
		this.url = url;
		this.handlers = handlers;
	}

	open(): Promise<void> {
		if (this.opened !== null) {
			return this.opened;
		}
		this.handlers.onStateChange("connecting");
		const { promise, resolve, reject } = Promise.withResolvers<void>();
		const socket = new WebSocket(this.url);
		this.socket = socket;
		this.failOpen = reject;
		socket.addEventListener("open", () => {
			this.failOpen = null;
			this.handlers.onStateChange("connected");
			resolve();
		});
		socket.addEventListener("message", (event) => {
			this.onMessage(event.data);
		});
		socket.addEventListener("error", () => {
			this.handlers.onError("websocket error");
		});
		socket.addEventListener("close", () => {
			this.finishClose();
		});
		this.opened = promise;
		return promise;
	}

	call<T>(
		method: string,
		params: JsonValue,
		parse: (value: unknown) => T,
	): Promise<T> {
		const socket = this.socket;
		if (socket === null || socket.readyState !== WebSocket.OPEN) {
			return Promise.reject(new Error("rpc client is not open"));
		}
		const id = this.nextId;
		this.nextId += 1;
		const { promise, resolve, reject } = Promise.withResolvers<T>();
		this.pending.set(id, {
			accept: (value) => {
				try {
					resolve(parse(value));
				} catch (error) {
					reject(
						error instanceof Error ? error : new Error("invalid rpc result"),
					);
				}
			},
			reject,
		});
		this.send({ jsonrpc: "2.0", id, method, params });
		return promise;
	}

	notify(method: string, params: JsonValue): void {
		this.send({ jsonrpc: "2.0", method, params });
	}

	respond(id: RequestId, result: JsonValue): void {
		this.send({ jsonrpc: "2.0", id, result });
	}

	close(): void {
		this.socket?.close();
	}

	private finishClose(): void {
		this.handlers.onStateChange("disconnected");
		const error = new Error("connection closed");
		const failOpen = this.failOpen;
		this.failOpen = null;
		if (failOpen !== null) {
			failOpen(error);
		}
		for (const entry of this.pending.values()) {
			entry.reject(error);
		}
		this.pending.clear();
		this.socket = null;
		this.opened = null;
	}

	private send(message: JsonValue): void {
		const socket = this.socket;
		if (socket === null || socket.readyState !== WebSocket.OPEN) {
			this.handlers.onError("rpc client is not open");
			return;
		}
		socket.send(JSON.stringify(message));
	}

	private onMessage(data: unknown): void {
		if (typeof data !== "string") {
			this.handlers.onError("rpc message is not text");
			return;
		}
		let parsed: unknown;
		try {
			parsed = JSON.parse(data);
		} catch {
			this.handlers.onError("rpc message is not json");
			return;
		}
		const message = rpcFields(parsed);
		if (message === null) {
			this.handlers.onError("rpc message is not an object");
			return;
		}
		if (message.jsonrpc !== undefined && message.jsonrpc !== "2.0") {
			this.handlers.onError("rpc version is not 2.0");
			return;
		}
		if (typeof message.method === "string") {
			this.onIncoming(message.method, message.id, message.params);
			return;
		}
		if (typeof message.id === "number") {
			this.onResponse(message.id, message);
		}
	}

	private onIncoming(
		method: string,
		id: JsonValue | undefined,
		params: JsonValue | undefined,
	): void {
		if (id === undefined) {
			try {
				const notification = parseNotification({ method, params });
				if (notification !== null) {
					this.handlers.onNotification(notification);
				}
			} catch (error) {
				this.handlers.onError(
					error instanceof Error ? error.message : "invalid notification",
				);
			}
			return;
		}
		if (typeof id !== "number" && typeof id !== "string") {
			this.handlers.onError("rpc request id is invalid");
			return;
		}
		if (method === questionMethod) {
			const question = parseQuestionRequest(id, params);
			if (question === null) {
				this.reject(id, -32602, "invalid params");
				this.handlers.onError("invalid question request");
				return;
			}
			this.handlers.onQuestion(question);
			return;
		}
		let request: ApprovalRequest | null;
		try {
			request = parseApprovalRequest(method, id, params);
		} catch (error) {
			this.reject(id, -32602, "invalid params");
			this.handlers.onError(
				error instanceof Error ? error.message : "invalid request",
			);
			return;
		}
		if (request !== null) {
			this.handlers.onRequest(request);
			return;
		}
		const knownMethod =
			method === "item/commandExecution/requestApproval" ||
			method === "item/fileChange/requestApproval";
		this.reject(
			id,
			knownMethod ? -32602 : -32601,
			knownMethod ? "invalid params" : "method not found",
		);
	}

	reject(id: RequestId, code: number, message: string): void {
		this.send({ jsonrpc: "2.0", id, error: { code, message } });
	}

	private onResponse(id: number, message: RpcFields): void {
		const pending = this.pending.get(id);
		if (pending === undefined) {
			return;
		}
		this.pending.delete(id);
		if (message.hasError) {
			pending.reject(rpcError(message.error));
			return;
		}
		if (!message.hasResult) {
			pending.reject(new Error("rpc response has no result"));
			return;
		}
		pending.accept(message.result);
	}
}
