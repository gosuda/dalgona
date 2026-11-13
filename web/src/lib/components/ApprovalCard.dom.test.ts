import { cleanup, fireEvent, render, within } from "@testing-library/svelte";
import { afterEach, describe, expect, type Mock, test, vi } from "vitest";
import ApprovalCard from "#lib/components/ApprovalCard.svelte";
import type { ApprovalRequest, FileChange } from "#lib/types.ts";

afterEach(cleanup);

const commandRequest: ApprovalRequest = {
	id: "req-1",
	method: "item/commandExecution/requestApproval",
	threadId: "thread-1",
	turnId: "turn-1",
	itemId: "run-1/2",
	title: "Run ls",
	detail: "ls",
	reason: null,
};

const fileRequest: ApprovalRequest = {
	id: "req-2",
	method: "item/fileChange/requestApproval",
	threadId: "thread-1",
	turnId: "turn-1",
	itemId: "fc-2",
	title: "Apply changes",
	detail: "",
	reason: null,
};

const rewrite: FileChange = {
	path: "b.txt",
	kind: "update",
	diff: "--- a/b.txt\n+++ b/b.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n",
	additions: 1,
	deletions: 1,
	diffOmitted: false,
};

function props(request: ApprovalRequest, changes: readonly FileChange[] = []) {
	return {
		request,
		changes,
		position: 1,
		total: 1,
		subagent: null,
		onDecision: vi.fn(),
	};
}

function requestDetails(container: HTMLElement): HTMLElement {
	const details = container.querySelector<HTMLElement>(
		'[aria-label="Request details"]',
	);
	if (details === null) throw new Error("no request details region");
	return details;
}

function mockScrolling(details: HTMLElement) {
	const scrollBy = vi.fn();
	const scrollTo = vi.fn();
	const metrics = { scrollBy, scrollTo, scrollHeight: 1000, clientHeight: 200 };
	for (const [name, value] of Object.entries(metrics)) {
		Object.defineProperty(details, name, { value, configurable: true });
	}
	return { scrollBy, scrollTo };
}

function lastTop(scroll: Mock): number {
	const options = scroll.mock.lastCall?.[0] as { top: number } | undefined;
	if (options === undefined) throw new Error("the details did not scroll");
	return options.top;
}

describe("approval card", () => {
	test("keeps the actions outside the scrolling details", () => {
		const { container } = render(ApprovalCard, {
			props: props(commandRequest),
		});
		const details = container.querySelector<HTMLElement>(
			'[aria-label="Request details"]',
		);
		if (details === null) throw new Error("no request details region");
		expect(
			within(details).queryByRole("button", { name: "Accept" }),
		).toBeNull();
		expect(container.querySelector(".request-actions")?.contains(details)).toBe(
			false,
		);
	});

	test("y accepts and Escape cancels and interrupts", async () => {
		const request = commandRequest;
		const onDecision = vi.fn();
		const { container } = render(ApprovalCard, {
			props: { ...props(request), onDecision },
		});
		const card = container.querySelector("section");
		if (card === null) throw new Error("no approval card");
		await fireEvent.keyDown(card, { key: "y" });
		expect(onDecision).toHaveBeenLastCalledWith(request, "accept");
		await fireEvent.keyDown(card, { key: "Escape" });
		expect(onDecision).toHaveBeenLastCalledWith(request, "cancel");
	});

	test("ignores a shortcut pressed with Ctrl, Meta or Alt", () => {
		const onDecision = vi.fn();
		const { container } = render(ApprovalCard, {
			props: { ...props(commandRequest), onDecision },
		});
		const card = container.querySelector("section");
		if (card === null) throw new Error("no approval card");
		for (const modifier of ["ctrlKey", "metaKey", "altKey"] as const) {
			const chord = new KeyboardEvent("keydown", {
				key: "a",
				bubbles: true,
				cancelable: true,
				[modifier]: true,
			});
			const notHandled = card.dispatchEvent(chord);
			expect(notHandled, modifier).toBe(true);
		}
		expect(onDecision).not.toHaveBeenCalled();
	});

	test("scrolls the details from the title and the actions with the keyboard", async () => {
		const { container } = render(ApprovalCard, {
			props: props(commandRequest),
		});
		const { scrollBy, scrollTo } = mockScrolling(requestDetails(container));
		const title = container.querySelector("h2");
		if (title === null) throw new Error("no approval title");
		const accept = within(container).getByRole("button", { name: "Accept" });
		const cancel = within(container).getByRole("button", {
			name: "Cancel and interrupt",
		});

		expect(await fireEvent.keyDown(title, { key: "ArrowDown" })).toBe(false);
		const line = lastTop(scrollBy);
		expect(line).toBeGreaterThan(0);
		expect(await fireEvent.keyDown(title, { key: "ArrowUp" })).toBe(false);
		expect(lastTop(scrollBy)).toBe(-line);

		expect(await fireEvent.keyDown(accept, { key: "PageDown" })).toBe(false);
		const page = lastTop(scrollBy);
		expect(page).toBeGreaterThan(line);
		expect(page).toBeLessThanOrEqual(200);
		expect(await fireEvent.keyDown(cancel, { key: "PageUp" })).toBe(false);
		expect(lastTop(scrollBy)).toBe(-page);

		expect(await fireEvent.keyDown(title, { key: "Home" })).toBe(false);
		expect(lastTop(scrollTo)).toBe(0);
		expect(await fireEvent.keyDown(cancel, { key: "End" })).toBe(false);
		expect(lastTop(scrollTo)).toBe(1000);
	});

	test("leaves scrolling keys to the browser inside the details, with a modifier, and without overflow", async () => {
		const { container } = render(ApprovalCard, {
			props: props(commandRequest),
		});
		const details = requestDetails(container);
		const { scrollBy, scrollTo } = mockScrolling(details);
		const title = container.querySelector("h2");
		const command = details.querySelector("pre");
		if (title === null || command === null) throw new Error("no approval card");

		expect(await fireEvent.keyDown(command, { key: "PageDown" })).toBe(true);
		expect(
			await fireEvent.keyDown(title, { key: "PageDown", ctrlKey: true }),
		).toBe(true);
		Object.defineProperty(details, "scrollHeight", {
			value: 200,
			configurable: true,
		});
		expect(await fireEvent.keyDown(title, { key: "End" })).toBe(true);
		expect(scrollBy).not.toHaveBeenCalled();
		expect(scrollTo).not.toHaveBeenCalled();
	});

	test("shows a nested exec approval with its command and no eval name", () => {
		const { container } = render(ApprovalCard, {
			props: props(commandRequest),
		});
		expect(container.querySelector("h2")?.textContent).toBe(
			"Run this command?",
		);
		const details = container.querySelector<HTMLElement>(
			'[aria-label="Request details"]',
		);
		expect(details?.textContent).toContain("ls");
		expect(container.textContent).not.toContain("eval");
	});

	test("shows a rewrite change as a diff in a file-change approval", () => {
		const { container } = render(ApprovalCard, {
			props: props(fileRequest, [rewrite]),
		});
		expect(container.querySelector("h2")?.textContent).toBe(
			"Apply these changes?",
		);
		const cards = container.querySelectorAll<HTMLDetailsElement>(
			"details.file-change",
		);
		expect(cards).toHaveLength(1);
		expect(cards[0]?.open).toBe(true);
		const summary = cards[0]?.querySelector("summary")?.textContent ?? "";
		expect(summary).toContain("Changed");
		expect(summary).toContain("b.txt");
		expect(summary).toContain("+1 −1");
		expect(cards[0]?.querySelector("del")?.textContent).toBe("-beta");
		expect(cards[0]?.querySelector("ins")?.textContent).toBe("+gamma");
	});
});
