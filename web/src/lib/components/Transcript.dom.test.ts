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
		readOutput: vi.fn(),
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

describe("jump to latest", () => {
	function stubScroller(target?: HTMLElement): HTMLElement {
		const transcript = target ?? document.getElementById("transcript");
		if (transcript === null) throw new Error("transcript section missing");
		transcript.style.overflowY = "auto";
		Object.defineProperty(transcript, "scrollHeight", {
			configurable: true,
			value: 2000,
		});
		Object.defineProperty(transcript, "clientHeight", {
			configurable: true,
			value: 500,
		});
		Object.defineProperty(transcript, "scrollTop", {
			configurable: true,
			writable: true,
			value: 0,
		});
		Object.defineProperty(transcript, "scrollTo", {
			configurable: true,
			value: vi.fn(({ top }: ScrollToOptions) => {
				transcript.scrollTop = Math.min(Number(top), 1500);
			}),
		});
		return transcript;
	}

	test("offers the button after the reader scrolls away and returns to the end", async () => {
		const { container } = render(Transcript, {
			props: props([reply("m1", "partial answer", false)]),
		});
		const transcript = stubScroller();
		await fireEvent.scroll(transcript);
		const button = screen.getByRole("button", { name: /Jump to latest/ });
		const scrollTo = transcript.scrollTo as unknown as ReturnType<typeof vi.fn>;
		await fireEvent.click(button);
		expect(scrollTo).toHaveBeenCalledWith({ top: 2000, behavior: "auto" });
		expect(document.activeElement).toBe(
			container.querySelector(".items > li[tabindex='-1']"),
		);
		expect(screen.queryByRole("button", { name: /Jump to latest/ })).toBeNull();
		transcript.scrollTop = 1500;
		await fireEvent.scroll(transcript);
		expect(screen.queryByRole("button", { name: /Jump to latest/ })).toBeNull();
	});

	test("counts items that arrive while the reader is away", async () => {
		const first = reply("m1", "first", true);
		const { rerender } = render(Transcript, {
			props: props([first]),
		});
		const transcript = stubScroller();
		await fireEvent.scroll(transcript);
		await rerender({ ...props([first, reply("m2", "second", false)]) });
		expect(
			screen.getByRole("button", { name: "Jump to latest · 1 new" }),
		).not.toBeNull();
	});

	test("observes an ancestor scroll box and preserves the reader's place across streaming", async () => {
		const target = document.createElement("div");
		document.body.append(target);
		const ancestor = stubScroller(target);
		const { container, rerender, unmount } = render(Transcript, {
			target,
			props: props([reply("m1", "partial", false)]),
		});
		ancestor.scrollTop = 1100;
		await fireEvent.scroll(ancestor);
		expect(
			screen.getByRole("button", { name: /Jump to latest/ }),
		).not.toBeNull();
		await rerender(props([reply("m1", "more streamed text", false)]));
		expect(ancestor.scrollTop).toBe(1100);
		await rerender({
			...props([reply("m1", "finished", true), reply("m2", "last row", true)]),
			turnStatus: "completed",
		});
		await fireEvent.click(
			screen.getByRole("button", { name: "Jump to latest · 1 new" }),
		);
		expect(ancestor.scrollTop).toBe(1500);
		expect(document.activeElement).toBe(
			container.querySelector(".items > li:last-child"),
		);
		expect(screen.queryByRole("button", { name: /Jump to latest/ })).toBeNull();
		unmount();
		target.remove();
	});

	async function checkAwayAfterTurnFinished(
		stubOwner: (host: HTMLElement) => HTMLElement,
	): Promise<void> {
		const target = document.createElement("div");
		const outside = document.createElement("button");
		document.body.append(target, outside);
		const jump = () => screen.queryByRole("button", { name: /Jump to latest/ });
		try {
			const { container, unmount } = render(Transcript, {
				target,
				props: {
					...props([reply("m1", "all of the answer", true)]),
					turnStatus: "completed",
				},
			});
			const scroller = stubOwner(target);
			outside.focus();
			scroller.scrollTop = 548;
			await fireEvent.scroll(scroller);
			expect(jump()?.textContent).toBe("Jump to latest");
			expect(document.activeElement).toBe(outside);
			await fireEvent.click(jump() as HTMLElement);
			expect(scroller.scrollTop).toBe(1500);
			expect(document.activeElement).toBe(
				container.querySelector(".items > li:last-child"),
			);
			expect(jump()).toBeNull();
			scroller.scrollTop = 300;
			await fireEvent.scroll(scroller);
			expect(jump()).not.toBeNull();
			unmount();
		} finally {
			target.remove();
			outside.remove();
		}
	}

	test("offers the button after the turn finished when the transcript itself scrolls", async () => {
		await checkAwayAfterTurnFinished(() => stubScroller());
	});

	test("offers the button after the turn finished when a scrolling ancestor owns the scroll", async () => {
		await checkAwayAfterTurnFinished((host) => stubScroller(host));
	});
});
