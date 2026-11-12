import { render } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import ApprovalDialog from "#lib/components/ApprovalDialog.svelte";
import type { ApprovalRequest } from "#lib/types.ts";

const request: ApprovalRequest = {
	id: "req-1",
	method: "item/commandExecution/requestApproval",
	threadId: "thread-1",
	turnId: "turn-1",
	itemId: "cmd-1",
	title: "Run ls",
	detail: "ls",
	reason: null,
};

function appliedRule(selector: string): CSSStyleRule {
	for (const sheet of Array.from(document.styleSheets)) {
		for (const rule of Array.from(sheet.cssRules)) {
			if (
				rule instanceof CSSStyleRule &&
				rule.selectorText.includes(selector)
			) {
				return rule;
			}
		}
	}
	throw new Error(`no applied rule for ${selector}`);
}

describe("approval kicker", () => {
	afterEach(() => {
		document.body.replaceChildren();
	});

	test("the kicker takes the point role", () => {
		render(ApprovalDialog, {
			props: { request, changes: [], onDecision: vi.fn() },
		});
		expect(appliedRule(".kicker").style.getPropertyValue("color")).toBe(
			"var(--point)",
		);
	});
});
