import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import CommandPalette from "#lib/components/CommandPalette.svelte";

afterEach(() => {
	cleanup();
	document.body.replaceChildren();
});

function field(): HTMLTextAreaElement {
	const node = document.createElement("textarea");
	document.body.append(node);
	return node;
}

describe("command palette", () => {
	test("arrow keys move the active option and Enter opens its panel", () => {
		const node = field();
		const onOpen = vi.fn();
		const onClose = vi.fn();
		render(CommandPalette, {
			props: {
				id: "command-list",
				field: node,
				query: "",
				active: 0,
				onOpen,
				onClose,
			},
		});
		node.dispatchEvent(
			new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
		);
		node.dispatchEvent(
			new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
		);
		expect(onOpen).toHaveBeenCalledWith("settings");
		expect(onClose).not.toHaveBeenCalled();
	});

	test("Tab opens the panel of the highlighted command", () => {
		const node = field();
		const onOpen = vi.fn();
		render(CommandPalette, {
			props: {
				id: "command-list",
				field: node,
				query: "re",
				active: 0,
				onOpen,
				onClose: vi.fn(),
			},
		});
		node.dispatchEvent(
			new KeyboardEvent("keydown", { key: "Tab", bubbles: true }),
		);
		expect(onOpen).toHaveBeenCalledWith("resume");
	});

	test("arrow keys wrap around the matching commands", () => {
		const node = field();
		const onOpen = vi.fn();
		render(CommandPalette, {
			props: {
				id: "command-list",
				field: node,
				query: "re",
				active: 0,
				onOpen,
				onClose: vi.fn(),
			},
		});
		node.dispatchEvent(
			new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }),
		);
		node.dispatchEvent(
			new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
		);
		expect(onOpen).toHaveBeenCalledWith("review");
	});

	test("Escape closes the list and keeps the draft", () => {
		const node = field();
		const onClose = vi.fn();
		const onOpen = vi.fn();
		render(CommandPalette, {
			props: {
				id: "command-list",
				field: node,
				query: "",
				active: 0,
				onOpen,
				onClose,
			},
		});
		node.dispatchEvent(
			new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
		);
		expect(onClose).toHaveBeenCalledOnce();
		expect(onOpen).not.toHaveBeenCalled();
	});

	test("a query that matches no command says so and opens nothing", () => {
		const node = field();
		const onOpen = vi.fn();
		render(CommandPalette, {
			props: {
				id: "command-list",
				field: node,
				query: "zz",
				active: 0,
				onOpen,
				onClose: vi.fn(),
			},
		});
		expect(
			screen.getByText(
				"No command named zz. Start with // to send a message that begins with /.",
			),
		).toBeTruthy();
		node.dispatchEvent(
			new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
		);
		expect(onOpen).not.toHaveBeenCalled();
	});
});
