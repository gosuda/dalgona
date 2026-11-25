import "../../app.css";
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import NoticeStack from "#lib/components/NoticeStack.svelte";
import { appliedRule, ruleSelectors } from "#lib/css-rule.ts";

function elementStyle(id: string): CSSStyleDeclaration {
	const element = document.getElementById(id);
	if (!element) throw new Error(`no element has id ${id}`);
	return getComputedStyle(element);
}

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

	test("sits after the base button rule in app.css", () => {
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
		const rule = appliedRule(
			'button:not(:disabled):not([aria-disabled="true"]):not(.point):hover',
		);
		expect(rule.style.getPropertyValue("border-color")).toBe("var(--strong)");
		expect(rule.style.getPropertyValue("background")).toBe("var(--tone)");
	});

	test("hover skips aria-disabled controls, which keep the disabled styling", () => {
		const rule = appliedRule('button:is(:disabled, [aria-disabled="true"])');
		expect(rule.style.getPropertyValue("cursor")).toBe("not-allowed");
		expect(rule.style.getPropertyValue("color")).toBe("var(--dim)");
		expect(rule.style.getPropertyValue("border-color")).toBe("var(--line)");
		const hover = appliedRule(
			'button:not(:disabled):not([aria-disabled="true"]):not(.point):hover',
		);
		expect(hover.style.getPropertyValue("border-color")).toBe("var(--strong)");
		const root = document.documentElement;
		root.style.setProperty("--dim", "rgb(1, 2, 3)");
		root.style.setProperty("--line", "rgb(4, 5, 6)");
		root.style.setProperty("--tone", "rgb(7, 8, 9)");
		try {
			document.body.innerHTML =
				'<button id="disabled-control" aria-disabled="true">Disabled</button>';
			const style = elementStyle("disabled-control");
			expect(style.color).toBe("rgb(1, 2, 3)");
			expect(style.borderColor).toBe("rgb(4, 5, 6)");
		} finally {
			document.body.innerHTML = "";
			root.style.removeProperty("--dim");
			root.style.removeProperty("--line");
			root.style.removeProperty("--tone");
		}
	});
});

describe("disabled point button", () => {
	test("a busy point button takes the disabled styling", () => {
		const root = document.documentElement;
		root.style.setProperty("--dim", "rgb(1, 2, 3)");
		root.style.setProperty("--line", "rgb(4, 5, 6)");
		root.style.setProperty("--tone", "rgb(7, 8, 9)");
		root.style.setProperty("--point", "rgb(10, 11, 12)");
		root.style.setProperty("--on-point", "rgb(13, 14, 15)");
		try {
			document.body.innerHTML =
				'<button id="busy-point" class="point" aria-disabled="true">Save</button><button id="live-point" class="point">Save</button>';
			const busy = elementStyle("busy-point");
			expect(busy.color).toBe("rgb(1, 2, 3)");
			expect(busy.borderColor).toBe("rgb(4, 5, 6)");
			expect(busy.backgroundColor).toBe("rgb(7, 8, 9)");
			const live = elementStyle("live-point");
			expect(live.color).toBe("rgb(13, 14, 15)");
			expect(live.backgroundColor).toBe("rgb(10, 11, 12)");
		} finally {
			document.body.innerHTML = "";
			root.style.removeProperty("--dim");
			root.style.removeProperty("--line");
			root.style.removeProperty("--tone");
			root.style.removeProperty("--point");
			root.style.removeProperty("--on-point");
		}
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
		const rule = appliedRule(
			'button.quiet:is(:disabled, [aria-disabled="true"])',
		);
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
