import "../../app.css";
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import NoticeStack from "#lib/components/NoticeStack.svelte";
import { appliedRule, ruleSelectors } from "#lib/css-rule.ts";

describe("button type", () => {
	const selector =
		"button:not(.thread, .thread-choice, .model-button, .request-actions button)";

	test("takes the Button row of DESIGN.md: 650, 14 px, line height 1, inline flex, centred, no wrap", () => {
		const rule = appliedRule(selector);
		expect(rule.parentRule).toBeNull();
		expect(rule.style.getPropertyValue("font-weight")).toBe(
			"var(--weight-control)",
		);
		expect(rule.style.getPropertyValue("font-size")).toBe("var(--size-label)");
		expect(rule.style.getPropertyValue("line-height")).toBe(
			"var(--leading-tight)",
		);
		expect(rule.style.getPropertyValue("display")).toBe("inline-flex");
		expect(rule.style.getPropertyValue("align-items")).toBe("center");
		expect(rule.style.getPropertyValue("justify-content")).toBe("center");
		expect(rule.style.getPropertyValue("white-space")).toBe("nowrap");
	});

	test("comes after the base button rule, so its label type wins", () => {
		const selectors = ruleSelectors();
		expect(selectors.indexOf(selector)).toBeGreaterThan(
			selectors.indexOf("button"),
		);
	});
});

describe("notice buttons", () => {
	afterEach(() => {
		cleanup();
		document.documentElement.style.removeProperty("--size-label");
		document.documentElement.style.removeProperty("--size-caption");
	});

	test("Dismiss takes the Button label size, not the caption size", () => {
		document.documentElement.style.setProperty("--size-label", "14px");
		document.documentElement.style.setProperty("--size-caption", "12px");
		render(NoticeStack, {
			props: {
				notices: [{ id: 1, tone: "error", text: "Failed.", actions: [] }],
				waiting: { count: 0, threadId: null },
				onDismiss: vi.fn(),
				onOpenWaiting: vi.fn(),
			},
		});
		const dismiss = screen.getByRole("button", { name: "Dismiss" });
		expect(getComputedStyle(dismiss).fontSize).toBe("14px");
	});
});

describe("button hover", () => {
	test("hover uses the mapped border and tone, and leaves the point fill alone", () => {
		const rule = appliedRule("button:not(:disabled):not(.point):hover");
		expect(rule.style.getPropertyValue("border-color")).toBe("var(--strong)");
		expect(rule.style.getPropertyValue("background")).toBe("var(--tone)");
	});
});

describe("quiet button", () => {
	test("rests with no fill, no drawn edge and a dim label", () => {
		const rule = appliedRule("button.quiet");
		expect(rule.style.getPropertyValue("border-color")).toBe("transparent");
		expect(rule.style.getPropertyValue("background")).toBe("transparent");
		expect(rule.style.getPropertyValue("color")).toBe("var(--dim)");
	});

	test("takes the quiet padding of 12 px, and leaves the danger variant its own", () => {
		const rule = appliedRule("button.quiet:not(.danger)");
		expect(rule.style.getPropertyValue("padding")).toBe("0px var(--space-3)");
	});

	test("disabled keeps the tone fill, the line edge and the dim label", () => {
		const rule = appliedRule("button.quiet:disabled");
		expect(rule.style.getPropertyValue("border-color")).toBe("var(--line)");
		expect(rule.style.getPropertyValue("background")).toBe("var(--tone)");
		expect(rule.style.getPropertyValue("color")).toBe("var(--dim)");
	});

	test("danger comes after quiet, so Interrupt keeps its failed edge and label", () => {
		const selectors = ruleSelectors();
		const quiet = selectors.indexOf("button.quiet");
		expect(quiet).toBeGreaterThanOrEqual(0);
		expect(selectors.indexOf("button.danger")).toBeGreaterThan(quiet);
	});
});

describe("disclosure summary target", () => {
	test("each summary keeps the minimum pointer target height", () => {
		const rule = appliedRule("summary");
		expect(rule.parentRule).toBeNull();
		expect(rule.style.getPropertyValue("min-height")).toBe("var(--target-min)");
	});
});
