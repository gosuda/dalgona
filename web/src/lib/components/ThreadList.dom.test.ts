import "../../app.css";
import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import ThreadList from "#lib/components/ThreadList.svelte";
import { threadHue } from "#lib/hue.ts";
import type { ThreadSummary } from "#lib/types.ts";

const thread: ThreadSummary = {
	id: "thread-1",
	title: "Plan",
	preview: "Write the parser for the config files",
	status: "idle",
	updatedAt: 1,
	model: null,
	cwd: "/work",
};

function props(
	threads: readonly ThreadSummary[],
	extra: {
		activeId?: string | null;
		waitingThreadIds?: ReadonlySet<string>;
		completedThreadIds?: ReadonlySet<string>;
	} = {},
) {
	return {
		threads,
		activeId: extra.activeId ?? null,
		loading: false,
		connected: true,
		disabled: false,
		waitingThreadIds: extra.waitingThreadIds ?? new Set<string>(),
		completedThreadIds: extra.completedThreadIds ?? new Set<string>(),
		onNew: vi.fn(),
		onSelect: vi.fn(),
	};
}

describe("thread list typography", () => {
	afterEach(() => {
		cleanup();
	});

	test("the thread count is a tabular count", () => {
		const { container } = render(ThreadList, {
			props: props([thread, { ...thread, id: "thread-2" }]),
		});
		expect(container.querySelector(".count")?.textContent).toBe("2");
	});

	test("previews keep words whole", () => {
		const { container } = render(ThreadList, { props: props([thread]) });
		const preview = container.querySelector(".preview");
		expect(preview?.classList.contains("prose")).toBe(true);
		const style = getComputedStyle(preview as Element);
		expect(style.wordBreak).toBe("keep-all");
		expect(style.overflowWrap).toBe("anywhere");
	});
});

describe("thread list selection", () => {
	afterEach(() => {
		cleanup();
	});

	test("marks only the open thread with aria-current", () => {
		const second = { ...thread, id: "thread-2", title: "Review" };
		const { container } = render(ThreadList, {
			props: props([thread, second], { activeId: "thread-2" }),
		});
		const rows = [...container.querySelectorAll("button.thread")];
		expect(rows).toHaveLength(2);
		expect(rows[0]?.getAttribute("aria-current")).toBeNull();
		expect(rows[1]?.getAttribute("aria-current")).toBe("true");
		expect(container.querySelectorAll('[aria-current="true"]')).toHaveLength(1);
		expect(container.querySelector(".selected")).toBeNull();
	});

	test("shows Waiting for a thread with a pending request", () => {
		const { container } = render(ThreadList, {
			props: props([thread], { waitingThreadIds: new Set(["thread-1"]) }),
		});
		expect(container.querySelector("button.thread")?.textContent).toContain(
			"Waiting",
		);
	});

	test("gives each row a hue dot that the text does not depend on", () => {
		const { container } = render(ThreadList, { props: props([thread]) });
		const dot = container.querySelector(".hue-dot");
		expect(dot?.getAttribute("aria-hidden")).toBe("true");
		expect(dot?.getAttribute("data-hue")).toBe(String(threadHue("thread-1")));
	});
});

describe("thread list states", () => {
	afterEach(() => {
		cleanup();
	});

	test("shows Done for an idle thread whose last turn completed", () => {
		const { container } = render(ThreadList, {
			props: props([thread], { completedThreadIds: new Set(["thread-1"]) }),
		});
		expect(container.querySelector("button.thread .tag")?.textContent).toBe(
			"Done",
		);
	});

	test("shows Ready for an idle thread without a completed turn", () => {
		const { container } = render(ThreadList, { props: props([thread]) });
		expect(container.querySelector("button.thread .tag")?.textContent).toBe(
			"Ready",
		);
	});

	test("shows the date after the state tag, not above it", () => {
		const { container } = render(ThreadList, { props: props([thread]) });
		const row = container.querySelector("button.thread") as HTMLElement;
		const tag = row.querySelector(".tag") as Element;
		const date = row.querySelector("time") as Element;
		expect(row.querySelector(".thread-topline time")).toBeNull();
		expect(
			tag.compareDocumentPosition(date) & Node.DOCUMENT_POSITION_FOLLOWING,
		).toBeTruthy();
	});
});
