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
	estimatedContextTokens: null,
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

function rowItems(container: HTMLElement): string[][] {
	return [...container.querySelectorAll(".band > dl.row")].map((row) =>
		[...row.querySelectorAll("[data-item]")].map(
			(item) => item.getAttribute("data-item") ?? "",
		),
	);
}

function dropConditions(item: string): string[][] {
	const found: string[][] = [];
	const walk = (list: CSSRuleList, path: string[]): void => {
		for (const rule of Array.from(list)) {
			if (rule instanceof CSSStyleRule) {
				if (
					rule.selectorText.replace(/\.svelte-\w+/g, "") ===
						`[data-item="${item}"]` &&
					rule.style.getPropertyValue("display") === "none"
				) {
					found.push(path);
				}
			} else if ("cssRules" in rule) {
				const condition = (rule as CSSConditionRule).conditionText;
				walk((rule as CSSGroupingRule).cssRules, [...path, condition]);
			}
		}
	};
	for (const sheet of Array.from(document.styleSheets)) {
		walk(sheet.cssRules, []);
	}
	return found;
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

	test("puts the items in three rows", () => {
		const { container } = render(StatusBand, {
			props: props({ queued: 1 }),
		});
		expect(rowItems(container)).toEqual([
			["model"],
			["effort", "approval", "sandbox", "folder", "queue"],
			["context", "mark"],
		]);
	});

	test("drops items by band width only in the one-row band at 40rem or narrower", () => {
		render(StatusBand, { props: props({ queued: 1 }) });
		expect(dropConditions("folder")).toEqual([
			["(max-width: 40rem)", "band (max-width: 30rem)"],
		]);
		expect(dropConditions("queue")).toEqual([
			["(max-width: 40rem)", "band (max-width: 26rem)"],
		]);
		expect(dropConditions("effort")).toEqual([
			["(max-width: 40rem)", "band (max-width: 16rem)"],
		]);
		expect(dropConditions("mark")).toEqual([
			["(max-width: 40rem)", "band (max-width: 12rem)"],
		]);
	});

	test("renders no row without an item", () => {
		const second = ["effort", "approval", "sandbox", "folder", "queue"];
		const markOnly = render(StatusBand, {
			props: props({ statusHide: second, usage: null }),
		});
		expect(rowItems(markOnly.container)).toEqual([["model"], ["mark"]]);
		const modelOnly = render(StatusBand, {
			props: props({ statusHide: [...second, "mark"], usage: null }),
		});
		expect(rowItems(modelOnly.container)).toEqual([["model"]]);
	});

	test("shows effort as a labelled glyph and context as a percent", () => {
		const { container } = render(StatusBand, { props: props() });
		expect(
			container.querySelector('[aria-label="Effort high"]'),
		).not.toBeNull();
		const high =
			container.querySelector('[aria-label="Effort high"]')?.textContent ?? "";
		expect([...high]).toHaveLength(1);
		const low =
			render(StatusBand, {
				props: props({ status: { ...status, effort: "low" } }),
			}).container.querySelector('[aria-label="Effort low"]')?.textContent ??
			"";
		expect([...low]).toHaveLength(1);
		expect(high).not.toBe(low);
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
