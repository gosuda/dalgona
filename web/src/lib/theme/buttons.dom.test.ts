import "../../app.css";
import { describe, expect, test } from "vitest";

function styleRules(list: CSSRuleList): CSSStyleRule[] {
	return Array.from(list).flatMap((rule) => {
		if (rule instanceof CSSStyleRule) return [rule];
		if ("cssRules" in rule) {
			return styleRules((rule as CSSGroupingRule).cssRules);
		}
		return [];
	});
}

function appliedRule(selector: string): CSSStyleRule {
	for (const sheet of Array.from(document.styleSheets)) {
		const rule = styleRules(sheet.cssRules).find(
			(candidate) => candidate.selectorText === selector,
		);
		if (rule) return rule;
	}
	throw new Error(`no applied rule for ${selector}`);
}

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
		const selectors = Array.from(document.styleSheets).flatMap((sheet) =>
			styleRules(sheet.cssRules).map((rule) => rule.selectorText),
		);
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
