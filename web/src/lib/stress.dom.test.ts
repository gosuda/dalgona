import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import App from "#lib/components/App.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type { AppNotification, JsonValue, RpcHandlers } from "#lib/types.ts";

const location = {
	hash: "#token=abc",
	host: "localhost:7437",
	pathname: "/",
	protocol: "http:",
	search: "",
};

class RecordingObserver implements ResizeObserver {
	static instances: RecordingObserver[] = [];
	static observed = new Map<Element, number>();

	readonly callback: ResizeObserverCallback;
	disconnected = false;
	deliveries = 0;

	constructor(callback: ResizeObserverCallback) {
		this.callback = callback;
		RecordingObserver.instances.push(this);
	}

	observe(target: Element): void {
		RecordingObserver.observed.set(
			target,
			(RecordingObserver.observed.get(target) ?? 0) + 1,
		);
	}

	unobserve(target: Element): void {
		const count = RecordingObserver.observed.get(target) ?? 0;
		if (count <= 1) RecordingObserver.observed.delete(target);
		else RecordingObserver.observed.set(target, count - 1);
	}

	disconnect(): void {
		this.disconnected = true;
	}

	flood(target: Element, times: number): void {
		for (let i = 0; i < times; i += 1) {
			this.deliveries += 1;
			this.callback([{ target } as ResizeObserverEntry], this);
		}
	}
}

const listenerCounts = new Map<string, { added: number; removed: number }>();
let originalAdd: EventTarget["addEventListener"];
let originalRemove: EventTarget["removeEventListener"];

function countListeners(): Record<string, number> {
	const out: Record<string, number> = {};
	for (const [type, tally] of listenerCounts) {
		out[type] = tally.added - tally.removed;
	}
	return out;
}

function startListenerCounting(): void {
	originalAdd = EventTarget.prototype.addEventListener;
	originalRemove = EventTarget.prototype.removeEventListener;
	EventTarget.prototype.addEventListener = function (
		this: EventTarget,
		type: string,
		listener: EventListenerOrEventListenerObject,
		options?: boolean | AddEventListenerOptions,
	) {
		const tally = listenerCounts.get(type) ?? { added: 0, removed: 0 };
		tally.added += 1;
		listenerCounts.set(type, tally);
		return originalAdd.call(this, type, listener, options);
	} as typeof EventTarget.prototype.addEventListener;
	EventTarget.prototype.removeEventListener = function (
		this: EventTarget,
		type: string,
		listener: EventListenerOrEventListenerObject,
		options?: boolean | EventListenerOptions,
	) {
		const tally = listenerCounts.get(type) ?? { added: 0, removed: 0 };
		tally.removed += 1;
		listenerCounts.set(type, tally);
		return originalRemove.call(this, type, listener, options);
	} as typeof EventTarget.prototype.removeEventListener;
}

function stopListenerCounting(): void {
	EventTarget.prototype.addEventListener = originalAdd;
	EventTarget.prototype.removeEventListener = originalRemove;
	listenerCounts.clear();
}

class StressPort implements RpcPort {
	private readonly handlers: RpcHandlers;
	private readonly table: Record<string, (params: JsonValue) => JsonValue>;
	private resolveOpen: (() => void) | null = null;

	closed = false;

	constructor(
		handlers: RpcHandlers,
		table: Record<string, (params: JsonValue) => JsonValue> = {},
	) {
		this.handlers = handlers;
		this.table = table;
	}

	open(): Promise<void> {
		this.handlers.onStateChange("connecting");
		const { promise, resolve } = Promise.withResolvers<void>();
		this.resolveOpen = resolve;
		return promise;
	}

	finishOpen(): void {
		this.handlers.onStateChange("connected");
		this.resolveOpen?.();
		this.resolveOpen = null;
	}

	async call<T>(
		method: string,
		params: JsonValue,
		parse: (value: unknown) => T,
	): Promise<T> {
		const answer = this.table[method];
		if (answer === undefined) {
			throw new Error(`no answer for ${method}`);
		}
		return parse(answer(params));
	}

	notify(): void {}

	respond(): void {}

	close(): void {
		this.closed = true;
	}

	drop(): void {
		this.handlers.onStateChange("disconnected");
	}

	deliver(notification: AppNotification): void {
		this.handlers.onNotification(notification);
	}
}

function baseAnswers(): Record<string, (params: JsonValue) => JsonValue> {
	return {
		initialize: () => ({
			userAgent: "dalgona/0.1.0",
			platformFamily: "linux",
			platformOs: "linux",
		}),
		"thread/list": () => ({
			data: [
				{
					id: "t1",
					name: "one",
					preview: "",
					status: "idle",
					updatedAt: 1,
					cwd: "/work",
				},
			],
			nextCursor: null,
		}),
		"model/list": () => ({ data: [] }),
		"config/read": () => ({
			config: { model: "gpt-test", origins: {} },
			origins: {},
		}),
		"dalgona/keywords/list": () => [],
		"dalgona/settings/read": () => ({ settings: [] }),
		"dalgona/advisor/status": () => [],
		"thread/resume": () => ({
			thread: {
				id: "t1",
				updatedAt: 1,
				status: "idle",
				cwd: "/work",
				turns: [
					{
						id: "turn-1",
						status: "inProgress",
						items: [
							{
								id: "item-user",
								type: "userMessage",
								content: [{ type: "text", text: "hi" }],
							},
							{
								id: "item-agent",
								type: "agentMessage",
								text: "",
							},
							{
								id: "item-tool",
								type: "commandExecution",
								command: "ls",
								aggregatedOutput: "file",
								status: "completed",
								exitCode: 0,
							},
						],
					},
				],
			},
			model: "gpt-test",
			approvalsReviewer: "user",
			modelProvider: "openai",
			approvalPolicy: "on-request",
			sandbox: "workspace-write",
			cwd: "/work",
		}),
	};
}

async function settle(turns = 80): Promise<void> {
	for (let i = 0; i < turns; i += 1) await Promise.resolve();
}

async function frame(): Promise<void> {
	vi.useFakeTimers();
	try {
		await vi.advanceTimersByTimeAsync(50);
	} finally {
		vi.useRealTimers();
	}
}

async function appWithThread(
	overrides: Record<string, (params: JsonValue) => JsonValue> = {},
): Promise<{ session: Session; ports: StressPort[] }> {
	const ports: StressPort[] = [];
	const session = new Session((_url, handlers) => {
		const port = new StressPort(handlers, {
			...baseAnswers(),
			...overrides,
		});
		ports.push(port);
		return port;
	});
	session.start(location);
	await settle();
	ports[0]?.finishOpen();
	await settle();
	await session.selectThread("t1");
	await settle();
	render(App, { props: { session } });
	await settle();
	await frame();
	return { session, ports };
}

function stubScroller(): {
	element: HTMLElement;
	scrollTo: ReturnType<typeof vi.fn>;
} {
	const transcript = document.getElementById("transcript");
	if (transcript === null) throw new Error("transcript section missing");
	transcript.style.overflowY = "auto";
	Object.defineProperty(transcript, "scrollHeight", {
		configurable: true,
		value: 2000,
	});
	Object.defineProperty(transcript, "clientHeight", {
		configurable: true,
		value: 500,
	});
	Object.defineProperty(transcript, "scrollTop", {
		configurable: true,
		writable: true,
		value: 0,
	});
	const scrollTo = vi.fn((options: { top: number }) => {
		Object.defineProperty(transcript, "scrollTop", {
			configurable: true,
			writable: true,
			value: options.top,
		});
	});
	Object.defineProperty(transcript, "scrollTo", {
		configurable: true,
		value: scrollTo,
	});
	return { element: transcript, scrollTo };
}

function installDockHeight(): void {
	Object.defineProperty(HTMLElement.prototype, "clientHeight", {
		configurable: true,
		get(this: HTMLElement) {
			return this.classList.contains("dock") ? 240 : 0;
		},
	});
}

function removeDockHeight(): void {
	Reflect.deleteProperty(HTMLElement.prototype, "clientHeight");
}

function agentDelta(index: number, itemId = "item-agent"): AppNotification {
	return {
		type: "delta",
		channel: "agent",
		threadId: "t1",
		turnId: "turn-1",
		itemId,
		delta: `${index}|`,
	};
}

function deliver(port: StressPort, list: readonly AppNotification[]): void {
	for (const notification of list) port.deliver(notification);
}

beforeEach(() => {
	vi.stubGlobal("ResizeObserver", RecordingObserver);
	RecordingObserver.observed = new Map();
});

afterEach(() => {
	cleanup();
	removeDockHeight();
	vi.unstubAllGlobals();
	vi.useRealTimers();
});

describe("mount cycles", () => {
	test("forty cycles leak no listeners and no observer registrations", async () => {
		installDockHeight();
		startListenerCounting();
		try {
			for (let cycle = 0; cycle < 40; cycle += 1) {
				const session = new Session(
					(_url, handlers) => new StressPort(handlers),
				);
				const { unmount } = render(App, { props: { session } });
				await settle();
				window.dispatchEvent(new Event("resize"));
				document.dispatchEvent(new Event("scroll"));
				unmount();
			}
			await settle();
			const net = countListeners();
			for (const [type, count] of Object.entries(net)) {
				expect(count, `listener type ${type}`).toBe(0);
			}
			expect(RecordingObserver.observed.size).toBe(0);
			expect(RecordingObserver.instances.length).toBeGreaterThan(0);
		} finally {
			stopListenerCounting();
		}
	});
});

describe("resize and observer storms", () => {
	test("storms leave layout state equal to a fresh mount", async () => {
		installDockHeight();
		const made = await appWithThread();
		const { session, ports } = made;
		const port = ports[0] as StressPort;
		const scroller = stubScroller();
		const widths = [0, 1, 320, 10000];
		for (const width of widths) {
			window.innerWidth = width;
			window.dispatchEvent(new Event("resize"));
		}
		const dock = document.querySelector<HTMLElement>(".dock");
		expect(dock).not.toBeNull();
		expect(RecordingObserver.instances.length).toBeGreaterThan(0);
		const shell = document.querySelector<HTMLElement>(".shell");
		expect(shell?.style.getPropertyValue("--dock-height")).toBe("240px");

		scroller.element.scrollTop = 2000;
		scroller.element.dispatchEvent(new Event("scroll"));
		await settle();
		expect(document.querySelector(".jump")).toBeNull();

		scroller.element.scrollTop = 0;
		scroller.element.dispatchEvent(new Event("scroll"));
		await settle();
		expect(screen.getByText(/^Jump to latest/)).toBeTruthy();

		deliver(port, [
			{
				type: "itemCompleted",
				threadId: "t1",
				turnId: "turn-1",
				item: {
					id: "item-extra-1",
					kind: "agentMessage",
					text: "extra one",
					complete: true,
				},
			},
			{
				type: "itemCompleted",
				threadId: "t1",
				turnId: "turn-1",
				item: {
					id: "item-extra-2",
					kind: "agentMessage",
					text: "extra two",
					complete: true,
				},
			},
			{
				type: "itemCompleted",
				threadId: "t1",
				turnId: "turn-1",
				item: {
					id: "item-extra-3",
					kind: "agentMessage",
					text: "extra three",
					complete: true,
				},
			},
		]);
		await settle();
		await frame();
		expect(screen.getByText("Jump to latest · 3 new")).toBeTruthy();

		for (let i = 0; i < 5000; i += 1) {
			window.innerWidth = widths[i % widths.length] ?? 0;
			window.dispatchEvent(new Event("resize"));
		}
		for (const observer of RecordingObserver.instances) {
			if (dock) observer.flood(dock, 20000);
		}
		await settle();
		expect(screen.getByText("Jump to latest · 3 new")).toBeTruthy();
		expect(shell?.style.getPropertyValue("--dock-height")).toBe("240px");

		const jump = screen.getByText(/^Jump to latest/) as HTMLButtonElement;
		jump.click();
		await settle();
		await frame();
		expect(document.querySelector(".jump")).toBeNull();
		expect(scroller.scrollTo).toHaveBeenCalled();
		expect(scroller.element.scrollTop).toBe(2000);

		const settledRows = document.querySelectorAll(".items > li").length;
		const settledHeight = shell?.style.getPropertyValue("--dock-height");
		cleanup();

		const fresh = await appWithThread();
		fresh.ports[0]?.deliver({
			type: "itemCompleted",
			threadId: "t1",
			turnId: "turn-1",
			item: {
				id: "item-extra-1",
				kind: "agentMessage",
				text: "extra one",
				complete: true,
			},
		});
		fresh.ports[0]?.deliver({
			type: "itemCompleted",
			threadId: "t1",
			turnId: "turn-1",
			item: {
				id: "item-extra-2",
				kind: "agentMessage",
				text: "extra two",
				complete: true,
			},
		});
		fresh.ports[0]?.deliver({
			type: "itemCompleted",
			threadId: "t1",
			turnId: "turn-1",
			item: {
				id: "item-extra-3",
				kind: "agentMessage",
				text: "extra three",
				complete: true,
			},
		});
		await settle();
		await frame();
		const freshShell = document.querySelector<HTMLElement>(".shell");
		expect(freshShell?.style.getPropertyValue("--dock-height")).toBe(
			settledHeight,
		);
		expect(document.querySelectorAll(".items > li").length).toBe(settledRows);
		expect(session.notices.length).toBe(0);
	});

	test("scroll storms keep the follow state stable", async () => {
		installDockHeight();
		await appWithThread();
		const scroller = stubScroller();
		for (let i = 0; i < 3000; i += 1) {
			scroller.element.scrollTop = i % 2 === 0 ? 0 : 2000;
			scroller.element.dispatchEvent(new Event("scroll"));
		}
		scroller.element.scrollTop = 2000;
		scroller.element.dispatchEvent(new Event("scroll"));
		await settle();
		await frame();
		expect(document.querySelector(".jump")).toBeNull();
	});
});

describe("delta storms", () => {
	test("eight thousand deltas concatenate once in order inside a bounded DOM", async () => {
		installDockHeight();
		const made = await appWithThread();
		const { session, ports } = made;
		const port = ports[0] as StressPort;
		const chunks: string[] = [];
		const storm: AppNotification[] = [];
		for (let i = 0; i < 8000; i += 1) {
			const chunk = `${i}|`;
			chunks.push(chunk);
			storm.push(agentDelta(i));
		}
		deliver(port, storm);
		await settle();
		await frame();
		const expected = chunks.join("");
		const rows = document.querySelectorAll(".items > li");
		expect(rows.length).toBe(3);
		const agentText = document.querySelector(
			".agent-message .prose",
		)?.textContent;
		expect(agentText?.replace(/\n/g, "")).toBe(expected);
		for (let i = 0; i < 40; i += 1) {
			session.notify("info", `notice ${i}`);
		}
		await settle();
		expect(session.notices.length).toBe(5);
		const rendered = document.querySelectorAll(".notice").length;
		expect(rendered).toBe(5);
		const nodesBefore = document.querySelectorAll("*").length;
		deliver(
			port,
			Array.from({ length: 3000 }, (_, i) => agentDelta(8000 + i)),
		);
		await settle();
		await frame();
		expect(document.querySelectorAll(".items > li").length).toBe(3);
		expect(document.querySelectorAll(".notice").length).toBe(5);
		expect(document.querySelectorAll("*").length).toBe(nodesBefore);
	});

	test("mutations per burst stay bounded", async () => {
		installDockHeight();
		const made = await appWithThread();
		const port = made.ports[0] as StressPort;
		await settle();
		await frame();
		const transcript = document.getElementById("transcript");
		expect(transcript).not.toBeNull();
		let mutations = 0;
		const observer = new MutationObserver((records) => {
			mutations += records.length;
		});
		observer.observe(transcript as HTMLElement, {
			childList: true,
			characterData: true,
			subtree: true,
		});
		deliver(
			port,
			Array.from({ length: 5000 }, (_, i) => agentDelta(i)),
		);
		await settle();
		await frame();
		observer.disconnect();
		expect(mutations).toBeGreaterThan(0);
		expect(mutations).toBeLessThanOrEqual(2 * 5000 + 100);
	});
});

describe("status flicker", () => {
	test("rapid turn toggles leave no stale spinner or notices", async () => {
		installDockHeight();
		const made = await appWithThread();
		const { session, ports } = made;
		const port = ports[0] as StressPort;
		expect(document.querySelectorAll(".tag.live").length).toBeGreaterThan(0);
		const storm: AppNotification[] = [];
		for (let i = 0; i < 2000; i += 1) {
			storm.push({
				type: "turnStarted",
				threadId: "t1",
				turn: {
					id: `turn-flip-${i}`,
					status: "inProgress",
					items: [],
					error: null,
				},
			});
			storm.push({
				type: "turnCompleted",
				threadId: "t1",
				turn: {
					id: `turn-flip-${i}`,
					status: "completed",
					items: [],
					error: null,
				},
			});
		}
		for (let i = 0; i < 2000; i += 1) {
			storm.push({
				type: "requestResolved",
				requestId: `missing-${i}`,
				resolution: null,
			});
		}
		deliver(port, storm);
		await settle();
		await frame();
		expect(document.querySelectorAll(".tag.live").length).toBe(0);
		expect(document.querySelectorAll(".dot").length).toBe(0);
		expect(
			document.querySelector(".thread-title .tag")?.textContent?.trim(),
		).toBe("Done");
		expect(document.querySelector("[role=status] .sr-only")?.textContent).toBe(
			"Done",
		);
		expect(session.notices.length).toBe(0);
		expect(document.querySelectorAll(".notice").length).toBe(0);
	});

	test("a manual reconnect shows Offline and then restores the state", async () => {
		installDockHeight();
		const made = await appWithThread();
		const { session, ports } = made;
		const port = ports[0] as StressPort;
		const tagText = () =>
			document.querySelector(".thread-title .tag")?.textContent?.trim();
		expect(tagText()).toBe("Working");
		port.drop();
		await settle();
		expect(tagText()).toBe("Offline");
		session.connect();
		await settle();
		expect(tagText()).toBe("Offline");
		ports.at(-1)?.finishOpen();
		await settle();
		await frame();
		expect(session.connection).toBe("connected");
		expect(tagText()).toBe("Working");
		expect(session.notices.length).toBe(0);
		expect(document.querySelectorAll(".notice").length).toBe(0);
	});
});
