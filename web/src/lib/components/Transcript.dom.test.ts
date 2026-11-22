import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, describe, expect, type Mock, test, vi } from "vitest";
import Transcript from "#lib/components/Transcript.svelte";
import type { TranscriptItem, TurnStatus } from "#lib/types.ts";

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

interface AnnounceInput {
	turnStatus?: TurnStatus | null;
	error?: string | null;
	retryRemainingMs?: number | null;
	awaitingApproval?: ReadonlySet<string>;
	loading?: boolean;
	reviewNote?: string | null;
}

interface AnnounceCase {
	name: string;
	input: AnnounceInput;
	want: string;
}

function scrolledAwayPage(scrollTop: number): () => void {
	const page = document.scrollingElement;
	if (page === null) throw new Error("no scrolling element");
	Object.defineProperty(page, "scrollHeight", {
		configurable: true,
		value: 5000,
	});
	Object.defineProperty(page, "clientHeight", {
		configurable: true,
		value: 800,
	});
	Object.defineProperty(page, "scrollTop", {
		configurable: true,
		writable: true,
		value: scrollTop,
	});
	return () => {
		Reflect.deleteProperty(page, "scrollHeight");
		Reflect.deleteProperty(page, "clientHeight");
		Reflect.deleteProperty(page, "scrollTop");
	};
}

function recorder() {
	const announced: string[] = [];
	const onAnnounce = (text: string): void => {
		announced.push(text);
	};
	return { announced, onAnnounce };
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

	test("mounts no live region while it shows a failed turn and a waiting request", () => {
		const { container } = render(Transcript, {
			props: {
				...props([reply("m1", "partial answer", false)]),
				turnStatus: "failed",
				error: "upstream reset",
				awaitingApproval: new Set(["m1"]),
			},
		});
		const live = "[role=log],[role=status],[role=alert],[aria-live]";
		expect(container.querySelector(live)).toBeNull();
		container.insertAdjacentHTML("beforeend", '<p role="status"></p>');
		expect(container.querySelector(live)).not.toBeNull();
	});
});

describe("turn announcement", () => {
	test("announces a turn change once", async () => {
		const { announced, onAnnounce } = recorder();
		const view = render(Transcript, {
			props: { ...props([reply("m1", "one", false)]), onAnnounce },
		});
		await tick();
		for (const text of [
			"one two",
			"one two three",
			"one two three four",
			"one two three four five",
			"one two three four five six",
		]) {
			await view.rerender({
				...props([reply("m1", text, false)]),
				onAnnounce,
			});
		}
		expect(announced).toEqual(["Working"]);

		await view.rerender({
			...props([reply("m1", "one two three four five six", true)]),
			turnStatus: "completed",
			onAnnounce,
		});
		expect(announced).toEqual(["Working", "Done"]);
	});

	test("a retry countdown is announced once", async () => {
		const { announced, onAnnounce } = recorder();
		const view = render(Transcript, {
			props: { ...props([]), retryRemainingMs: 90000, onAnnounce },
		});
		await tick();
		for (const ms of [89000, 88000, 60000]) {
			await view.rerender({
				...props([]),
				retryRemainingMs: ms,
				onAnnounce,
			});
		}
		expect(announced).toEqual(["Rate limited, retrying"]);

		await view.rerender({ ...props([]), retryRemainingMs: 0, onAnnounce });
		expect(announced).toEqual(["Rate limited, retrying", "Working"]);
	});

	const cases: readonly AnnounceCase[] = [
		{
			name: "a retry wait",
			input: { turnStatus: "inProgress", retryRemainingMs: 90000 },
			want: "Rate limited, retrying",
		},
		{
			name: "a waiting request",
			input: { turnStatus: "inProgress", awaitingApproval: new Set(["m1"]) },
			want: "Waiting for your reply",
		},
		{
			name: "a failed turn",
			input: { turnStatus: "failed", error: "upstream reset" },
			want: "Failed: upstream reset",
		},
		{
			name: "an interrupted turn",
			input: { turnStatus: "interrupted" },
			want: "Stopped",
		},
		{
			name: "a loading transcript",
			input: { turnStatus: null, loading: true },
			want: "Loading transcript…",
		},
		{ name: "no turn", input: { turnStatus: null }, want: "" },
	];

	test.each(cases)("announces $name", async ({ input, want }) => {
		const { announced, onAnnounce } = recorder();
		render(Transcript, {
			props: { ...props([]), ...input, onAnnounce },
		});
		await tick();
		expect(announced).toEqual([want]);
	});

	test("announces the review note above Working", async () => {
		const { announced, onAnnounce } = recorder();
		render(Transcript, {
			props: {
				...props([]),
				turnStatus: "inProgress",
				reviewNote: "Flagged: removed code",
				onAnnounce,
			},
		});
		await tick();
		expect(announced).toEqual(["Flagged: removed code"]);
	});

	test("no review note keeps the Working announcement", async () => {
		const { announced, onAnnounce } = recorder();
		render(Transcript, {
			props: {
				...props([]),
				turnStatus: "inProgress",
				reviewNote: null,
				onAnnounce,
			},
		});
		await tick();
		expect(announced).toEqual(["Working"]);
	});

	test("a waiting request outranks the review note", async () => {
		const { announced, onAnnounce } = recorder();
		render(Transcript, {
			props: {
				...props([]),
				turnStatus: "inProgress",
				awaitingApproval: new Set(["m1"]),
				reviewNote: "Failed: judge unreachable",
				onAnnounce,
			},
		});
		await tick();
		expect(announced).toEqual(["Waiting for your reply"]);
	});

	test("a failed turn outranks the review note", async () => {
		const { announced, onAnnounce } = recorder();
		render(Transcript, {
			props: {
				...props([]),
				turnStatus: "failed",
				error: "upstream reset",
				reviewNote: "Flagged: removed code",
				onAnnounce,
			},
		});
		await tick();
		expect(announced).toEqual(["Failed: upstream reset"]);
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
	interface StubbedScroller {
		readonly element: HTMLElement;
		readonly scrollTo: Mock<(options: ScrollToOptions) => void>;
	}

	function stubScroller(
		target?: HTMLElement,
		clientHeight = 500,
	): StubbedScroller {
		const transcript = target ?? document.getElementById("transcript");
		if (transcript === null) throw new Error("transcript section missing");
		transcript.style.overflowY = "auto";
		Object.defineProperty(transcript, "scrollHeight", {
			configurable: true,
			value: 2000,
		});
		Object.defineProperty(transcript, "clientHeight", {
			configurable: true,
			value: clientHeight,
		});
		Object.defineProperty(transcript, "scrollTop", {
			configurable: true,
			writable: true,
			value: 0,
		});
		const scrollTo = vi.fn(({ top }: ScrollToOptions) => {
			transcript.scrollTop = Math.min(Number(top), 2000 - clientHeight);
		});
		Object.defineProperty(transcript, "scrollTo", {
			configurable: true,
			value: scrollTo,
		});
		return { element: transcript, scrollTo };
	}

	test("offers the button after the reader scrolls away and returns to the end", async () => {
		const { container } = render(Transcript, {
			props: props([reply("m1", "partial answer", false)]),
		});
		const { element: transcript, scrollTo } = stubScroller();
		await fireEvent.scroll(transcript);
		const button = screen.getByRole("button", { name: /Jump to latest/ });
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

	test("click scrolls to the end and focuses the last row without moving the scroll", async () => {
		const { container } = render(Transcript, {
			props: props([
				reply("m1", "partial answer", false),
				reply("m2", "final answer", true),
			]),
		});
		const { element: transcript, scrollTo } = stubScroller();
		await fireEvent.scroll(transcript);
		const rows = container.querySelectorAll<HTMLElement>(".items > li");
		const row = rows[rows.length - 1];
		if (row === undefined) throw new Error("last row missing");
		const focus = vi.spyOn(row, "focus");
		await fireEvent.click(
			screen.getByRole("button", { name: /Jump to latest/ }),
		);
		expect(scrollTo).toHaveBeenCalledWith({ top: 2000, behavior: "auto" });
		expect(document.activeElement).toBe(row);
		expect(focus).toHaveBeenCalledWith({ preventScroll: true });
	});

	test("counts items that arrive while the reader is away", async () => {
		const first = reply("m1", "first", true);
		const { rerender } = render(Transcript, {
			props: props([first]),
		});
		const { element: transcript } = stubScroller();
		await fireEvent.scroll(transcript);
		await rerender({ ...props([first, reply("m2", "second", false)]) });
		expect(
			screen.getByRole("button", { name: "Jump to latest · 1 new" }),
		).not.toBeNull();
	});

	test("observes an ancestor scroll box and preserves the reader's place across streaming", async () => {
		const target = document.createElement("div");
		document.body.append(target);
		const { element: ancestor } = stubScroller(target, 400);
		try {
			const { rerender, unmount } = render(Transcript, {
				target,
				props: props([reply("m1", "partial", false)]),
			});
			ancestor.scrollTop = 1100;
			await fireEvent.scroll(ancestor);
			expect(
				screen.getByRole("button", { name: "Jump to latest" }),
			).not.toBeNull();
			await rerender({
				...props([
					reply("m1", "finished", true),
					reply("m2", "second", true),
					reply("m3", "third", true),
				]),
			});
			expect(ancestor.scrollTop).toBe(1100);
			expect(
				screen.getByRole("button", { name: "Jump to latest · 2 new" }),
			).not.toBeNull();
			ancestor.scrollTop = 1590;
			await fireEvent.scroll(ancestor);
			expect(
				screen.queryByRole("button", { name: /Jump to latest/ }),
			).toBeNull();
			await rerender({
				...props([
					reply("m1", "finished", true),
					reply("m2", "second", true),
					reply("m3", "third", true),
					reply("m4", "fourth", true),
				]),
			});
			const { promise: frame, resolve: nextFrame } = Promise.withResolvers();
			requestAnimationFrame(nextFrame);
			await frame;
			expect(ancestor.scrollTop).toBe(1600);
			expect(
				screen.queryByRole("button", { name: /Jump to latest/ }),
			).toBeNull();
			unmount();
		} finally {
			target.remove();
		}
	});

	async function checkAwayAfterTurnFinished(
		stubOwner: (host: HTMLElement) => StubbedScroller,
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
			const { element: scroller } = stubOwner(target);
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

describe("following the end", () => {
	interface GrowingScroller {
		readonly element: HTMLElement;
		readonly scrollTo: Mock<(options: ScrollToOptions) => void>;
		grow(by: number): void;
	}

	function growingScroller(clientHeight = 500): GrowingScroller {
		const transcript = document.getElementById("transcript");
		if (transcript === null) throw new Error("transcript section missing");
		let height = 1000;
		let top = 0;
		transcript.style.overflowY = "auto";
		Object.defineProperty(transcript, "scrollHeight", {
			configurable: true,
			get: () => height,
		});
		Object.defineProperty(transcript, "clientHeight", {
			configurable: true,
			value: clientHeight,
		});
		Object.defineProperty(transcript, "scrollTop", {
			configurable: true,
			get: () => top,
			set: (value: number) => {
				top = value;
			},
		});
		const scrollTo = vi.fn(({ top: to }: ScrollToOptions) => {
			top = Math.min(Number(to), height - clientHeight);
		});
		Object.defineProperty(transcript, "scrollTo", {
			configurable: true,
			value: scrollTo,
		});
		return {
			element: transcript,
			scrollTo,
			grow(by) {
				height += by;
			},
		};
	}

	async function frames(count: number): Promise<void> {
		for (let index = 0; index < count; index += 1) {
			const { promise, resolve } = Promise.withResolvers<void>();
			requestAnimationFrame(() => resolve());
			await promise;
		}
	}

	const jump = () => screen.queryByRole("button", { name: /Jump to latest/ });

	test("output that arrives before the scroll event of the follow scroll keeps following", async () => {
		const first = reply("m1", "first", true);
		const { rerender } = render(Transcript, { props: props([first]) });
		const scroller = growingScroller();
		await frames(2);
		expect(scroller.scrollTo).toHaveBeenCalledWith({
			top: 1000,
			behavior: "auto",
		});
		scroller.grow(100);
		await fireEvent.scroll(scroller.element);
		expect(jump()).toBeNull();
		await rerender({ ...props([first, reply("m2", "second", true)]) });
		await frames(2);
		expect(scroller.scrollTo).toHaveBeenLastCalledWith({
			top: 1100,
			behavior: "auto",
		});
	});

	test("a reader who scrolls up from the end leaves it", async () => {
		render(Transcript, { props: props([reply("m1", "first", true)]) });
		const scroller = growingScroller();
		await frames(2);
		scroller.grow(100);
		scroller.element.scrollTop = 300;
		await fireEvent.scroll(scroller.element);
		expect(jump()).not.toBeNull();
	});

	test("output that arrived before the last scroll event no longer counts after it", async () => {
		render(Transcript, { props: props([reply("m1", "first", true)]) });
		const scroller = growingScroller();
		await frames(2);
		scroller.grow(400);
		await fireEvent.scroll(scroller.element);
		expect(jump()).toBeNull();
		await fireEvent.scroll(scroller.element);
		expect(jump()).not.toBeNull();
	});

	test("a reader who scrolls up after Jump to latest leaves the end it reached", async () => {
		render(Transcript, { props: props([reply("m1", "first", true)]) });
		const scroller = growingScroller();
		await frames(2);
		scroller.element.scrollTop = 100;
		await fireEvent.scroll(scroller.element);
		expect(jump()).not.toBeNull();
		scroller.grow(500);
		await fireEvent.click(jump() as HTMLElement);
		expect(scroller.element.scrollTop).toBe(1000);
		scroller.element.scrollTop = 700;
		await fireEvent.scroll(scroller.element);
		expect(jump()).not.toBeNull();
	});

	test("a reader who is away stays away while output grew and the scroll lands near the old end", async () => {
		render(Transcript, { props: props([reply("m1", "first", true)]) });
		const scroller = growingScroller();
		await frames(2);
		scroller.element.scrollTop = 100;
		await fireEvent.scroll(scroller.element);
		expect(jump()).not.toBeNull();
		scroller.grow(500);
		scroller.element.scrollTop = 480;
		await fireEvent.scroll(scroller.element);
		expect(jump()).not.toBeNull();
	});

	test("the end is within 32 px and no further", async () => {
		render(Transcript, { props: props([reply("m1", "first", true)]) });
		const scroller = growingScroller();
		await frames(2);
		scroller.element.scrollTop = 469;
		await fireEvent.scroll(scroller.element);
		expect(jump()).toBeNull();
		scroller.element.scrollTop = 467;
		await fireEvent.scroll(scroller.element);
		expect(jump()).not.toBeNull();
	});

	test("the height remembered from one scroll container does not count in another", async () => {
		render(Transcript, { props: props([reply("m1", "first", true)]) });
		const scroller = growingScroller();
		await frames(2);
		scroller.element.style.overflowY = "visible";
		const restore = scrolledAwayPage(200);
		try {
			await fireEvent.scroll(document);
			expect(jump()).not.toBeNull();
		} finally {
			restore();
		}
	});

	test("scrolls nothing while no thread is open", async () => {
		const scrollTo = vi.fn();
		const page = document.createElement("div");
		Object.defineProperty(page, "scrollTo", { value: scrollTo });
		const spy = vi
			.spyOn(document, "scrollingElement", "get")
			.mockReturnValue(page);
		try {
			render(Transcript, {
				props: { ...props([]), threadId: null, turnStatus: null },
			});
			await frames(2);
			expect(scrollTo).not.toHaveBeenCalled();
		} finally {
			spy.mockRestore();
		}
	});

	test("shows no Jump to latest while no thread is open", async () => {
		render(Transcript, {
			props: { ...props([]), threadId: null, turnStatus: null },
		});
		await frames(2);
		const restore = scrolledAwayPage(0);
		try {
			await fireEvent.scroll(document);
			await fireEvent.scroll(document);
			expect(jump()).toBeNull();
		} finally {
			restore();
		}
	});

	test("reopening a thread after none was open follows its end", async () => {
		const items = [reply("m1", "first", true)];
		const { rerender } = render(Transcript, { props: props(items) });
		await frames(2);
		await rerender({ ...props([]), threadId: null, turnStatus: null });
		await frames(2);
		const restore = scrolledAwayPage(0);
		try {
			await fireEvent.scroll(document);
			await fireEvent.scroll(document);
		} finally {
			restore();
		}
		await rerender({ ...props(items) });
		const scroller = growingScroller();
		await frames(2);
		expect(scroller.scrollTo).toHaveBeenCalledWith({
			top: 1000,
			behavior: "auto",
		});
		expect(jump()).toBeNull();
	});
});
