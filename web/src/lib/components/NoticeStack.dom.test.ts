import { cleanup, render, screen, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import NoticeStack from "#lib/components/NoticeStack.svelte";
import Transcript from "#lib/components/Transcript.svelte";
import type {
	Notice,
	NoticeTone,
	TranscriptItem,
	TurnStatus,
	WaitingElsewhere,
} from "#lib/types.ts";

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

describe("turn announcement", () => {
	afterEach(cleanup);

	test("the transcript adds no status region of its own", () => {
		render(NoticeStack, { props: stack({ turnStatus: "inProgress" }) });
		render(Transcript, {
			props: transcript([reply("m1", "partial answer", false)], "inProgress"),
		});
		expect(screen.getAllByRole("status")).toHaveLength(1);
	});

	test("announces a turn change once", async () => {
		const stackView = render(NoticeStack, {
			props: stack({ turnStatus: "inProgress" }),
		});
		const transcriptView = render(Transcript, {
			props: transcript([reply("m1", "one", false)], "inProgress"),
		});
		const seen = [announced()];
		for (const text of [
			"one two",
			"one two three",
			"one two three four",
			"one two three four five",
			"one two three four five six",
		]) {
			await transcriptView.rerender(
				transcript([reply("m1", text, false)], "inProgress"),
			);
			seen.push(announced());
		}
		expect(new Set(seen)).toEqual(new Set(["Working"]));

		await stackView.rerender(stack({ turnStatus: "completed" }));
		expect(announced()).toBe("Done");
	});

	test("a retry countdown is announced once", async () => {
		const view = render(NoticeStack, {
			props: stack({ turnStatus: "inProgress", retryRemainingMs: 90000 }),
		});
		const seen = [announced()];
		for (const ms of [89000, 88000, 60000]) {
			await view.rerender(
				stack({ turnStatus: "inProgress", retryRemainingMs: ms }),
			);
			seen.push(announced());
		}
		expect(new Set(seen)).toEqual(new Set(["Rate limited, retrying"]));

		await view.rerender(
			stack({ turnStatus: "inProgress", retryRemainingMs: 0 }),
		);
		expect(announced()).toBe("Working");
	});

	test.each([
		{
			name: "a retry wait",
			input: { turnStatus: "inProgress" as const, retryRemainingMs: 90000 },
			want: "Rate limited, retrying",
		},
		{
			name: "a waiting request",
			input: {
				turnStatus: "inProgress" as const,
				awaitingApproval: new Set(["m1"]),
			},
			want: "Waiting for your reply",
		},
		{
			name: "a failed turn",
			input: { turnStatus: "failed" as const, turnError: "upstream reset" },
			want: "Failed: upstream reset",
		},
		{
			name: "an interrupted turn",
			input: { turnStatus: "interrupted" as const },
			want: "Stopped",
		},
		{
			name: "a loading transcript",
			input: { loadingTranscript: true },
			want: "Loading transcript…",
		},
		{ name: "no turn", input: {}, want: "" },
	])("announces $name", ({ input, want }) => {
		render(NoticeStack, { props: stack(input) });
		expect(announced()).toBe(want);
	});
});

interface TurnInput {
	turnStatus?: TurnStatus | null;
	turnError?: string | null;
	loadingTranscript?: boolean;
	retryRemainingMs?: number | null;
	awaitingApproval?: ReadonlySet<string>;
}

function stack(input: TurnInput) {
	return {
		notices: [],
		waiting: none,
		onDismiss: vi.fn(),
		onOpenWaiting: vi.fn(),
		...input,
	};
}

function transcript(items: readonly TranscriptItem[], turnStatus: TurnStatus) {
	return {
		threadId: "thread-1",
		items,
		turnStatus,
		error: null,
		plan: null,
		connected: true,
		keywords: [],
		loading: false,
		awaitingApproval: new Set<string>(),
	};
}

function reply(id: string, text: string, complete: boolean): TranscriptItem {
	return { id, kind: "agentMessage", text, complete };
}

function announced(): string | null {
	return (
		screen.getByRole("status").querySelector(".sr-only")?.textContent ?? null
	);
}
