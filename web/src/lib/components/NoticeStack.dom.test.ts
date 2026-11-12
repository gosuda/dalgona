import { cleanup, render, screen, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import NoticeStack from "#lib/components/NoticeStack.svelte";
import type { Notice, NoticeTone, WaitingElsewhere } from "#lib/types.ts";

const none: WaitingElsewhere = { count: 0, threadId: null };

function notice(id: number, tone: NoticeTone, text: string): Notice {
	return { id, tone, text, actions: [] };
}
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

describe("notice regions", () => {
	afterEach(() => {
		cleanup();
		document.getElementById("prompt")?.remove();
	});

	test("keeps the alert region mounted before an error arrives", async () => {
		const view = render(NoticeStack, {
			props: {
				notices: [],
				waiting: none,
				onDismiss: vi.fn(),
				onOpenWaiting: vi.fn(),
			},
		});
		const region = screen.getByRole("alert");
		await view.rerender({
			notices: [notice(1, "error", "Unable to load threads: timeout.")],
			waiting: none,
			onDismiss: vi.fn(),
			onOpenWaiting: vi.fn(),
		});
		expect(screen.getByRole("alert")).toBe(region);
		expect(region.textContent).toContain("Unable to load threads: timeout.");
	});

	test("returns focus to the message field after Dismiss", async () => {
		const prompt = document.createElement("textarea");
		prompt.id = "prompt";
		document.body.append(prompt);
		const user = userEvent.setup();
		let notices: readonly Notice[] = [
			notice(1, "warning", "Turn interrupted."),
		];
		const onDismiss = (): void => {
			notices = [];
			void view.rerender({
				notices,
				waiting: none,
				onDismiss,
				onOpenWaiting: vi.fn(),
			});
		};
		const view = render(NoticeStack, {
			props: { notices, waiting: none, onDismiss, onOpenWaiting: vi.fn() },
		});
		await user.click(screen.getByRole("button", { name: "Dismiss" }));
		await vi.waitFor(() => {
			expect(document.activeElement).toBe(prompt);
		});
	});

	test("routes warnings and info to the status region", () => {
		render(NoticeStack, {
			props: {
				notices: [
					notice(1, "error", "Failed to save."),
					notice(2, "info", "Signed in."),
				],
				waiting: none,
				onDismiss: vi.fn(),
				onOpenWaiting: vi.fn(),
			},
		});
		expect(
			within(screen.getByRole("alert")).getByText("Failed to save."),
		).toBeDefined();
		expect(
			within(screen.getByRole("status")).getByText("Signed in."),
		).toBeDefined();
	});

	test("each notice tone takes its Design-record text role", () => {
		render(NoticeStack, {
			props: {
				notices: [
					notice(1, "error", "Failed to save."),
					notice(2, "warning", "Turn interrupted."),
					notice(3, "info", "Signed in."),
				],
				waiting: none,
				onDismiss: vi.fn(),
				onOpenWaiting: vi.fn(),
			},
		});
		const error = appliedRule(".notice-error");
		expect(error.style.getPropertyValue("color")).toBe("var(--failed-text)");
		expect(error.style.getPropertyValue("font-weight")).toBe(
			"var(--weight-strong)",
		);
		expect(appliedRule(".notice-warning").style.getPropertyValue("color")).toBe(
			"var(--warning-text)",
		);
		expect(appliedRule(".notice-info").style.getPropertyValue("color")).toBe(
			"var(--dim)",
		);
	});
});
