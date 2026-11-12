import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import SettingsPanel from "#lib/components/panels/SettingsPanel.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type { JsonValue, RpcHandlers } from "#lib/types.ts";

afterEach(() => {
	cleanup();
	document.body.replaceChildren();
});

type Answer = (params: JsonValue) => JsonValue;

interface Call {
	readonly method: string;
	readonly params: JsonValue;
}

class RecordingPort implements RpcPort {
	private readonly log: Call[] = [];
	private readonly handlers: RpcHandlers;
	private readonly table: Record<string, Answer>;

	constructor(handlers: RpcHandlers, table: Record<string, Answer>) {
		this.handlers = handlers;
		this.table = table;
	}

	get calls(): readonly Call[] {
		return this.log;
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
		return Promise.resolve(parse(answer(params)));
	}

	notify(_method: string, _params: JsonValue): void {}
	respond(_id: string, _result: JsonValue): void {}
	close(): void {}
}

function isRecord(
	value: JsonValue,
): value is { readonly [key: string]: JsonValue } {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isList(value: JsonValue | undefined): value is readonly JsonValue[] {
	return Array.isArray(value);
}

function editsOf(params: JsonValue): { keyPath: string; value: JsonValue }[] {
	if (!isRecord(params)) return [];
	const edits = params["edits"];
	if (!isList(edits)) return [];
	return edits.flatMap((edit) => {
		if (!isRecord(edit)) return [];
		const keyPath = edit["keyPath"];
		if (typeof keyPath !== "string") return [];
		return [{ keyPath, value: edit["value"] ?? null }];
	});
}

function savedSettings(
	saved: Record<string, JsonValue>,
	reads: Record<string, JsonValue> = saved,
): Record<string, Answer> {
	return {
		initialize: () => ({ userAgent: "dalgona/0.1.0", platformOs: "linux" }),
		"dalgona/settings/read": () => ({
			settings: Object.entries(reads).map(([key, value]) => ({ key, value })),
		}),
		"config/batchWrite": (params) => {
			for (const edit of editsOf(params)) saved[edit.keyPath] = edit.value;
			return { filePath: "/home/settings.toml", status: "ok", version: "v2" };
		},
	};
}

async function openSettings(
	initial: Record<string, JsonValue>,
	options: { readonly staleRead?: boolean } = {},
): Promise<{
	readonly session: Session;
	readonly ports: RecordingPort[];
	readonly applied: string[];
}> {
	const saved = { ...initial };
	const reads = options.staleRead === true ? { ...initial } : saved;
	const ports: RecordingPort[] = [];
	const applied: string[] = [];
	const dial = (_url: string, handlers: RpcHandlers): RpcPort => {
		const port = new RecordingPort(handlers, savedSettings(saved, reads));
		ports.push(port);
		return port;
	};
	const session = new Session(dial, (setting) => applied.push(setting));
	session.start({
		hash: "#token=test",
		host: "127.0.0.1:1",
		pathname: "/",
		protocol: "http:",
		search: "",
	});
	await vi.waitFor(() => {
		expect(session.settingValues).toHaveLength(Object.keys(initial).length);
	});
	session.openPanel("settings");
	render(SettingsPanel, {
		props: {
			session,
			open: true,
			onClose: () => session.closePanel(),
		},
	});
	return { session, ports, applied };
}

const baseline: Record<string, JsonValue> = {
	approval: "auto",
	verbosity: "medium",
	theme: "auto",
	statusHide: [],
	editReview: false,
};

function batchCalls(port: RecordingPort | undefined): JsonValue[] {
	return (port?.calls ?? [])
		.filter((call) => call.method === "config/batchWrite")
		.map((call) => call.params);
}

describe("settings panel", () => {
	test("saves only changed keys", async () => {
		const { session, ports } = await openSettings(baseline);
		await fireEvent.click(screen.getByLabelText("Yolo"));
		await fireEvent.click(screen.getByRole("button", { name: "Save" }));
		await vi.waitFor(() => {
			expect(batchCalls(ports[0])).toHaveLength(1);
		});
		expect(batchCalls(ports[0])).toEqual([
			{
				edits: [
					{ keyPath: "approval", value: "yolo", mergeStrategy: "replace" },
				],
			},
		]);
		expect(session.panel).toBeNull();
	});

	test("applies the theme after save", async () => {
		const { ports, applied } = await openSettings(baseline, {
			staleRead: true,
		});
		await fireEvent.click(screen.getByLabelText("Night"));
		await fireEvent.click(screen.getByRole("button", { name: "Save" }));
		await vi.waitFor(() => {
			expect(applied).toContain("night");
		});
		expect(batchCalls(ports[0])).toEqual([
			{
				edits: [{ keyPath: "theme", value: "night", mergeStrategy: "replace" }],
			},
		]);
	});

	test("hides a status item under the statusHide wire key", async () => {
		const { ports } = await openSettings(baseline);
		await fireEvent.click(screen.getByLabelText("Folder"));
		await fireEvent.click(screen.getByRole("button", { name: "Save" }));
		await vi.waitFor(() => {
			expect(batchCalls(ports[0])).toHaveLength(1);
		});
		expect(batchCalls(ports[0])).toEqual([
			{
				edits: [
					{
						keyPath: "statusHide",
						value: ["folder"],
						mergeStrategy: "replace",
					},
				],
			},
		]);
	});

	test("turns review edits on under the editReview wire key", async () => {
		const { ports } = await openSettings(baseline);
		await fireEvent.click(screen.getByLabelText("Review edits"));
		await fireEvent.click(screen.getByRole("button", { name: "Save" }));
		await vi.waitFor(() => {
			expect(batchCalls(ports[0])).toHaveLength(1);
		});
		expect(batchCalls(ports[0])).toEqual([
			{
				edits: [
					{ keyPath: "editReview", value: true, mergeStrategy: "replace" },
				],
			},
		]);
	});

	test("rejects an empty choice with a message", async () => {
		const { ports } = await openSettings({ ...baseline, approval: "" });
		await fireEvent.click(screen.getByRole("button", { name: "Save" }));
		expect(
			await screen.findByText("Choose a policy for approvals."),
		).toBeTruthy();
		expect(batchCalls(ports[0])).toHaveLength(0);
	});
});
