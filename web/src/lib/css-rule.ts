function styleRules(list: CSSRuleList): CSSStyleRule[] {
	return Array.from(list).flatMap((rule) => {
		if (rule instanceof CSSStyleRule) return [rule];
		if ("cssRules" in rule) {
			return styleRules((rule as CSSGroupingRule).cssRules);
		}
		return [];
	});
}

export function appliedRule(selector: string): CSSStyleRule {
	for (const sheet of Array.from(document.styleSheets)) {
		const rule = styleRules(sheet.cssRules).find(
			(candidate) =>
				candidate.selectorText.replace(/\.svelte-\w+/g, "") === selector,
		);
		if (rule) return rule;
	}
	throw new Error(`no applied rule for ${selector}`);
}

export function ruleSelectors(): string[] {
	return Array.from(document.styleSheets).flatMap((sheet) =>
		styleRules(sheet.cssRules).map((rule) => rule.selectorText),
	);
}
