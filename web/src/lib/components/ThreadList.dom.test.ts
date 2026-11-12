import "../../app.css";
import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import ThreadList from "#lib/components/ThreadList.svelte";
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

function props(threads: readonly ThreadSummary[]) {
	return {
		threads,
		activeId: null,
		loading: false,
		connected: true,
		disabled: false,
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
