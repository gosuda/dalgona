import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import NoticeStack from "#lib/components/NoticeStack.svelte";
import Transcript from "#lib/components/Transcript.svelte";
import { appliedRule } from "#lib/css-rule.ts";
import type {
	Notice,
	NoticeTone,
	TranscriptItem,
	WaitingElsewhere,
} from "#lib/types.ts";

const none: WaitingElsewhere = { count: 0, threadId: null };

function notice(id: number, tone: NoticeTone, text: string): Notice {
	return { id, tone, text, actions: [] };
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

	test.each(["Dismiss", "Try again"])(
		"returns focus to the message field when %s removes a notice",
		async (action) => {
			const prompt = document.createElement("textarea");
			prompt.id = "prompt";
			document.body.append(prompt);
			const user = userEvent.setup();
			const onDismiss = (): void => {
				void view.rerender({ notices: [] });
			};
			const notices: readonly Notice[] = [
				{
					...notice(1, "error", "Unable to load more threads."),
					actions: [{ label: "Try again", run: onDismiss }],
				},
			];
			const view = render(NoticeStack, {
				props: { notices, waiting: none, onDismiss, onOpenWaiting: vi.fn() },
			});
			screen.getByRole("button", { name: action }).focus();
			await user.keyboard("{Enter}");
			await vi.waitFor(() => {
				expect(document.activeElement).toBe(prompt);
			});
		},
	);

	test("routes warnings and info to the status region", () => {
		render(NoticeStack, {
			props: {
				notices: [
					notice(1, "error", "Failed to save."),
					notice(2, "info", "Signed in."),
					notice(3, "warning", "Turn interrupted."),
				],
				waiting: none,
				onDismiss: vi.fn(),
				onOpenWaiting: vi.fn(),
			},
		});
		const alert = screen.getByRole("alert").textContent;
		const status = screen.getByRole("status").textContent;
		expect(alert).toContain("Failed to save.");
		expect(alert).not.toContain("Turn interrupted.");
		expect(alert).not.toContain("Signed in.");
		expect(status).toContain("Turn interrupted.");
		expect(status).toContain("Signed in.");
		expect(status).not.toContain("Failed to save.");
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

describe("status announcement", () => {
	afterEach(cleanup);

	test("shows the announcement in the status region and not in the alert region", () => {
		render(NoticeStack, { props: stack("Waiting for your reply") });
		expect(announced()).toBe("Waiting for your reply");
		expect(screen.getByRole("alert").textContent).toBe("");
	});

	test("shows no announcement text when none is given", () => {
		render(NoticeStack, { props: stack("") });
		expect(announced()).toBe("");
	});

	test("replaces the announcement when the prop changes", async () => {
		const view = render(NoticeStack, { props: stack("Working") });
		await view.rerender(stack("Done"));
		expect(announced()).toBe("Done");
	});

	test("the transcript adds no status region of its own", () => {
		render(NoticeStack, { props: stack("Working") });
		expect(screen.getAllByRole("status")).toHaveLength(1);
		render(Transcript, {
			props: transcript([reply("m1", "partial answer", false)]),
		});
		expect(screen.getAllByRole("status")).toHaveLength(1);
	});
});

function stack(announcement: string) {
	return {
		notices: [],
		waiting: none,
		onDismiss: vi.fn(),
		onOpenWaiting: vi.fn(),
		announcement,
	};
}

function transcript(items: readonly TranscriptItem[]) {
	return {
		threadId: "thread-1",
		items,
		turnStatus: "inProgress" as const,
		error: null,
		plan: null,
		turnReview: null,
		connected: true,
		childThreadIds: [],
		threads: [],
		userAgent: null,
		starting: false,
		onOpenThread: () => undefined,
		onStart: () => undefined,
		readOutput: vi.fn(),
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
