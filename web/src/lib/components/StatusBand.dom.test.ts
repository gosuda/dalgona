import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import StatusBand from "#lib/components/StatusBand.svelte";
import type { ModelOption, StatusInfo, TokenUsage } from "#lib/types.ts";

const status: StatusInfo = {
	model: "gpt-test",
	approval: "auto",
	sandbox: "workspace-write",
	effort: "high",
};

const model: ModelOption = {
	id: "gpt-test",
	label: "GPT",
	description: "A model",
	hidden: false,
	efforts: ["low", "medium", "high"],
	defaultEffort: "medium",
	presets: [],
	inputModalities: ["text", "image"],
};

const usage: TokenUsage = {
	last: {
		totalTokens: 50_000,
		inputTokens: 50_000,
		cachedInputTokens: 0,
		cacheWriteInputTokens: 0,
		outputTokens: 0,
		reasoningOutputTokens: 0,
	},
	total: {
		totalTokens: 50_000,
		inputTokens: 50_000,
		cachedInputTokens: 0,
		cacheWriteInputTokens: 0,
		outputTokens: 0,
		reasoningOutputTokens: 0,
	},
	contextWindow: 200_000,
};

function props(
	overrides: Partial<{
		status: StatusInfo;
		models: readonly ModelOption[];
		usage: TokenUsage | null;
		statusHide: readonly string[];
		folder: string;
		queued: number;
	}> = {},
) {
	return {
		status,
		models: [model],
		selectedModel: "gpt-test",
		modelDisabled: false,
		usage,
		statusHide: [],
		folder: "app",
		queued: 0,
		onOpenModel: vi.fn(),
		...overrides,
	};
}

function expectGlyph(text: string | undefined, codePoint: number): void {
	const glyphs = [...(text ?? "")];
	expect(glyphs).toHaveLength(1);
	expect(glyphs[0]?.codePointAt(0)).toBe(codePoint);
}

describe("StatusBand", () => {
	afterEach(() => {
		cleanup();
	});

	test("shows effort and context as text", () => {
		const { container } = render(StatusBand, { props: props() });
		expect(
			container.querySelector('[aria-label="Effort high"]'),
		).not.toBeNull();
		expect(container.querySelector(".percent")?.textContent).toBe("25%");
	});

	test("lists every shown segment under Status details", () => {
		const { container } = render(StatusBand, {
			props: props({ queued: 2 }),
		});
		const details = container.querySelector("details.band-more")?.textContent;
		expect(details).toContain("model gpt-test");
		expect(details).toContain("effort high");
		expect(details).toContain("approval auto");
		expect(details).toContain("folder app");
		expect(details).toContain("2 follow-ups queued");
		expect(details).toContain("context 25% used");
	});

	test("shows the workspace-write sandbox glyph by code point", () => {
		const { container } = render(StatusBand, { props: props() });
		const sandbox = container.querySelector('[data-item="sandbox"] .glyph');
		expectGlyph(sandbox?.textContent ?? undefined, 0xf03d7);
	});

	test("shows the none glyph when the sandbox is off", () => {
		const { container } = render(StatusBand, {
			props: props({ status: { ...status, sandbox: "off" } }),
		});
		const sandbox = container.querySelector('[data-item="sandbox"] .glyph');
		expect(sandbox?.getAttribute("aria-label")).toBe("Sandbox off");
		expectGlyph(sandbox?.textContent ?? undefined, 0xf002a);
	});

	test("hides the items named in statusHide, and never model", () => {
		const { container } = render(StatusBand, {
			props: props({ statusHide: ["folder", "model", "effort"] }),
		});
		expect(container.querySelector('[data-item="folder"]')).toBeNull();
		expect(container.querySelector('[data-item="effort"]')).toBeNull();
		expect(container.querySelector('[data-item="model"]')).not.toBeNull();
	});

	test("omits context without a context window", () => {
		const { container } = render(StatusBand, {
			props: props({ usage: { ...usage, contextWindow: null } }),
		});
		expect(container.querySelector('[data-item="context"]')).toBeNull();
	});

	test("omits effort when the model has fewer than two levels", () => {
		const { container } = render(StatusBand, {
			props: props({ models: [{ ...model, efforts: ["high"] }] }),
		});
		expect(container.querySelector('[data-item="effort"]')).toBeNull();
	});
});
