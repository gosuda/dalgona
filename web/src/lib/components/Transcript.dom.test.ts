import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import Transcript from "#lib/components/Transcript.svelte";
import type { TranscriptItem } from "#lib/types.ts";

function props(items: readonly TranscriptItem[]) {
	return {
		threadId: "thread-1",
		items,
		turnStatus: "inProgress" as const,
		error: null,
		plan: null,
		turnReview: null,
		connected: true,
		loading: false,
		awaitingApproval: new Set<string>(),
		childThreadIds: [],
		threads: [],
		userAgent: null,
		starting: false,
		onOpenThread: vi.fn(),
		onStart: vi.fn(),
	};
}

function reply(id: string, text: string, complete: boolean): TranscriptItem {
	return { id, kind: "agentMessage", text, complete };
}

afterEach(cleanup);

describe("transcript live regions", () => {
	test("streamed text is outside every live region", () => {
		const { container } = render(Transcript, {
			props: props([reply("m1", "partial answer", false)]),
		});
		const message = container.querySelector(".agent-message .prose");
		expect(message).not.toBeNull();
		expect(
			message?.closest("[role=log],[role=status],[role=alert],[aria-live]"),
		).toBeNull();
	});
});

describe("welcome start button", () => {
	test("is shown offline and does not start a thread when clicked", async () => {
		const onStart = vi.fn();
		render(Transcript, {
			props: { ...props([]), threadId: null, connected: false, onStart },
		});
		const button = screen.getByRole("button", { name: "Start thread" });
		expect(button.getAttribute("aria-disabled")).toBe("true");
		await fireEvent.click(button);
		expect(onStart).not.toHaveBeenCalled();
	});

	test("keeps focus while a start is in flight", async () => {
		const onStart = vi.fn();
		render(Transcript, {
			props: { ...props([]), threadId: null, starting: true, onStart },
		});
		const button = screen.getByRole("button", { name: "Start thread" });
		button.focus();
		expect(button.hasAttribute("disabled")).toBe(false);
		expect(button.getAttribute("aria-disabled")).toBe("true");
		expect(document.activeElement).toBe(button);
		await fireEvent.click(button);
		expect(onStart).not.toHaveBeenCalled();
	});
});
