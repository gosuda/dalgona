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
