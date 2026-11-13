import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import ModelPanel from "#lib/components/panels/ModelPanel.svelte";
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

const efforts = [
	{ reasoningEffort: "low", description: "low" },
	{ reasoningEffort: "medium", description: "medium" },
	{ reasoningEffort: "high", description: "high" },
];

const models: JsonValue = {
	data: [
		{
			id: "gpt-a",
			model: "gpt-a",
			displayName: "GPT A",
			description: "The quick one",
			hidden: false,
			supportedReasoningEfforts: efforts,
			defaultReasoningEffort: "medium",
			presets: ["default"],
		},
		{
			id: "gpt-b",
			model: "gpt-b",
			displayName: "GPT B",
			description: "The careful one",
			hidden: false,
			supportedReasoningEfforts: efforts,
			defaultReasoningEffort: "high",
			presets: ["smol", "slow"],
		},
	],
};

interface Setup {
	readonly modelList: JsonValue;
	readonly effort: string | null;
}

async function openModels(setup: Setup): Promise<{
	readonly session: Session;
	readonly ports: RecordingPort[];
}> {
	const saved: Record<string, JsonValue> = {
		model: "gpt-a",
		model_reasoning_effort: setup.effort,
	};
	const ports: RecordingPort[] = [];
	const dial = (_url: string, handlers: RpcHandlers): RpcPort => {
		const port = new RecordingPort(handlers, {
			initialize: () => ({
				userAgent: "dalgona/0.1.0",
				platformFamily: "unix",
				platformOs: "linux",
			}),
			"model/list": () => setup.modelList,
			"config/read": () => ({
				config: { ...saved },
				origins: {},
			}),
			"config/batchWrite": () => ({
				filePath: "/home/config.toml",
				status: "ok",
				version: "v2",
			}),
		});
		ports.push(port);
		return port;
	};
	const session = new Session(dial);
	session.start({
		hash: "#token=test",
		host: "127.0.0.1:1",
		pathname: "/",
		protocol: "http:",
		search: "",
	});
	await vi.waitFor(() => {
		expect(session.selectedModel).toBe("gpt-a");
	});
	session.openPanel("model");
	render(ModelPanel, {
		props: {
			session,
			open: true,
			onClose: () => session.closePanel(),
		},
	});
	return { session, ports };
}

function batchCalls(port: RecordingPort | undefined): readonly JsonValue[] {
	return (port?.calls ?? [])
		.filter((call) => call.method === "config/batchWrite")
		.map((call) => call.params);
}

describe("model panel", () => {
	test("writes model and effort in one batch", async () => {
		const { session, ports } = await openModels({
			modelList: models,
			effort: "medium",
		});
		await vi.waitFor(() => {
			expect(screen.getByLabelText("GPT B")).toBeTruthy();
		});
		await fireEvent.click(screen.getByLabelText("GPT B"));
		await fireEvent.click(screen.getByLabelText("low"));
		await fireEvent.click(screen.getByRole("button", { name: "Use model" }));
		await vi.waitFor(() => {
			expect(batchCalls(ports[0])).toHaveLength(1);
		});
		expect(batchCalls(ports[0])).toEqual([
			{
				edits: [
					{ keyPath: "model", value: "gpt-b", mergeStrategy: "replace" },
					{
						keyPath: "model_reasoning_effort",
						value: "low",
						mergeStrategy: "replace",
					},
				],
			},
		]);
		await vi.waitFor(() => {
			expect(session.panel).toBeNull();
		});
	});

	test("selecting a preset fills model and effort", async () => {
		const { ports } = await openModels({ modelList: models, effort: "medium" });
		await vi.waitFor(() => {
			expect(screen.getByLabelText("smol · GPT B · high")).toBeTruthy();
		});
		await fireEvent.click(screen.getByLabelText("smol · GPT B · high"));
		expect((screen.getByLabelText("GPT B") as HTMLInputElement).checked).toBe(
			true,
		);
		expect((screen.getByLabelText("high") as HTMLInputElement).checked).toBe(
			true,
		);
		await fireEvent.click(screen.getByRole("button", { name: "Use model" }));
		await vi.waitFor(() => {
			expect(batchCalls(ports[0])).toHaveLength(1);
		});
		expect(batchCalls(ports[0])).toEqual([
			{
				edits: [
					{ keyPath: "model", value: "gpt-b", mergeStrategy: "replace" },
					{
						keyPath: "model_reasoning_effort",
						value: "high",
						mergeStrategy: "replace",
					},
				],
			},
		]);
	});

	test("the empty state leads to login", async () => {
		const { session } = await openModels({
			modelList: { data: [] },
			effort: null,
		});
		expect(
			screen.getByText("No models. Log in to a provider to list its models."),
		).toBeTruthy();
		await fireEvent.click(screen.getByRole("button", { name: "Log in" }));
		expect(session.panel).toBe("login");
	});

	test("keeps the panel open with a message for an effort the model does not offer", async () => {
		const { session, ports } = await openModels({
			modelList: models,
			effort: "xhigh",
		});
		await vi.waitFor(() => {
			expect(screen.getByLabelText("GPT A")).toBeTruthy();
		});
		await fireEvent.click(screen.getByRole("button", { name: "Use model" }));
		expect(await screen.findByText("Choose an effort for gpt-a.")).toBeTruthy();
		expect(batchCalls(ports[0])).toHaveLength(0);
		expect(session.panel).toBe("model");
	});

	test("a thread switch while the panel is open keeps the pending model", async () => {
		const { session, ports } = await openModels({
			modelList: models,
			effort: "medium",
		});
		await vi.waitFor(() => {
			expect(screen.getByLabelText("GPT B")).toBeTruthy();
		});
		await fireEvent.click(screen.getByLabelText("GPT B"));
		session.selectedModel = "gpt-x";
		await tick();
		expect((screen.getByLabelText("GPT B") as HTMLInputElement).checked).toBe(
			true,
		);
		await fireEvent.click(screen.getByRole("button", { name: "Use model" }));
		await vi.waitFor(() => {
			expect(batchCalls(ports[0])).toHaveLength(1);
		});
		expect(batchCalls(ports[0])).toEqual([
			expect.objectContaining({
				edits: expect.arrayContaining([
					{ keyPath: "model", value: "gpt-b", mergeStrategy: "replace" },
				]),
			}),
		]);
	});
});
