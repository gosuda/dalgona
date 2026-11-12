import { waitFor } from "@testing-library/dom";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import Composer from "#lib/components/Composer.svelte";

function baseProps() {
	return {
		disabled: false,
		running: false,
		followups: [] as readonly string[],
		onEditFollowup: vi.fn(() => undefined),
		onRemoveFollowup: vi.fn(),
		onSend: vi.fn(),
		onSteer: vi.fn(),
		onFollowup: vi.fn(),
		onInterrupt: vi.fn(),
	};
}

describe("composer focus", () => {
	afterEach(cleanup);

	test("keeps focus on the submit control when a turn starts", async () => {
		const view = render(Composer, { props: baseProps() });
		const submit = screen.getByRole("button", { name: "Send" });
		submit.focus();
		await view.rerender({ running: true });
		expect(screen.getByRole("button", { name: "Steer" })).toBe(submit);
		expect(document.activeElement).toBe(submit);
	});

	test("returns focus to the message field when Interrupt disappears", async () => {
		const view = render(Composer, {
			props: { ...baseProps(), running: true },
		});
		const interrupt = screen.getByRole("button", { name: "Interrupt turn" });
		interrupt.focus();
		await view.rerender({ running: false });
		await waitFor(() => {
			expect(document.activeElement).toBe(
				screen.getByRole("textbox", { name: "Message" }),
			);
		});
	});

	test("lists the server's keyword matches for the draft as words", async () => {
		const onMatch = vi.fn(() =>
			Promise.resolve([
				{ word: "ultrathink", byteRange: { start: 0, end: 10 } },
			]),
		);
		render(Composer, { props: { ...baseProps(), onMatch } });
		const field = screen.getByRole("textbox", { name: "Message" });
		await fireEvent.input(field, { target: { value: "ultrathink" } });
		await waitFor(() => {
			expect(document.querySelector(".keywords")?.textContent).toBe(
				"Keywords: ultrathink",
			);
		});
		expect(onMatch).toHaveBeenCalledWith("ultrathink");
	});
});

describe("follow-up queue", () => {
	afterEach(cleanup);

	test("Alt+Enter queues and the entry is listed", async () => {
		const onFollowup = vi.fn();
		const view = render(Composer, {
			props: { ...baseProps(), running: true, onFollowup },
		});
		const field = screen.getByRole("textbox", { name: "Message" });
		await fireEvent.input(field, { target: { value: "after the build" } });
		await fireEvent.keyDown(field, { key: "Enter", altKey: true });
		expect(onFollowup).toHaveBeenCalledWith("after the build");
		await view.rerender({ followups: ["after the build"] });
		expect(
			screen.getByRole("list", { name: "Queued follow-ups" }).textContent,
		).toContain("after the build");
	});

	test("Edit returns the text to the composer", async () => {
		const onEditFollowup = vi.fn(() => "second follow-up");
		render(Composer, {
			props: {
				...baseProps(),
				followups: ["first", "second follow-up"],
				onEditFollowup,
			},
		});
		await fireEvent.click(
			screen.getAllByRole("button", { name: "Edit" })[1] as HTMLElement,
		);
		expect(onEditFollowup).toHaveBeenCalledWith(1);
		expect(
			(screen.getByRole("textbox", { name: "Message" }) as HTMLTextAreaElement)
				.value,
		).toBe("second follow-up");
	});

	test("Remove drops only that entry", async () => {
		const onRemoveFollowup = vi.fn();
		render(Composer, {
			props: {
				...baseProps(),
				followups: ["a", "b", "c"],
				onRemoveFollowup,
			},
		});
		await fireEvent.click(
			screen.getAllByRole("button", { name: "Remove" })[1] as HTMLElement,
		);
		expect(onRemoveFollowup).toHaveBeenCalledWith(1);
	});
});
