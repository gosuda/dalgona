import { waitFor } from "@testing-library/dom";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import Composer from "#lib/components/Composer.svelte";

function baseProps() {
	return {
		disabled: false,
		running: false,
		followups: [] as readonly string[],
		history: [] as readonly string[],
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

describe("composer history", () => {
	afterEach(cleanup);

	test("ArrowUp at the start recalls the last message and ArrowDown restores the draft", async () => {
		render(Composer, {
			props: { ...baseProps(), history: ["first message", "last message"] },
		});
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "dr" } });
		field.setSelectionRange(0, 0);
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(field.value).toBe("last message");
		field.setSelectionRange(0, 0);
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(field.value).toBe("first message");
		field.setSelectionRange(field.value.length, field.value.length);
		await fireEvent.keyDown(field, { key: "ArrowDown" });
		expect(field.value).toBe("last message");
		field.setSelectionRange(field.value.length, field.value.length);
		await fireEvent.keyDown(field, { key: "ArrowDown" });
		expect(field.value).toBe("dr");
	});

	test("ArrowUp recalls again after a recall without moving the caret", async () => {
		render(Composer, {
			props: { ...baseProps(), history: ["first message", "last message"] },
		});
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		field.setSelectionRange(0, 0);
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(field.value).toBe("last message");
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(field.value).toBe("first message");
	});

	test("typing into a recalled message makes the edited text the draft", async () => {
		render(Composer, {
			props: { ...baseProps(), history: ["old"] },
		});
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		field.setSelectionRange(0, 0);
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(field.value).toBe("old");
		await fireEvent.input(field, { target: { value: "old edited" } });
		field.setSelectionRange(0, 0);
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(field.value).toBe("old");
		await fireEvent.keyDown(field, { key: "ArrowDown" });
		expect(field.value).toBe("old edited");
	});
});

describe("interrupt guard", () => {
	afterEach(() => {
		cleanup();
		vi.useRealTimers();
	});

	test("one Esc arms and shows the hint; a second Esc within the window interrupts", async () => {
		const onInterrupt = vi.fn();
		render(Composer, { props: { ...baseProps(), running: true, onInterrupt } });
		const field = screen.getByRole("textbox", { name: "Message" });
		await fireEvent.keyDown(field, { key: "Escape" });
		expect(onInterrupt).not.toHaveBeenCalled();
		expect(document.getElementById("composer-help")?.textContent).toBe(
			"Esc again to interrupt",
		);
		await fireEvent.keyDown(field, { key: "Escape" });
		expect(onInterrupt).toHaveBeenCalledTimes(1);
	});

	test("the hint clears after the window", async () => {
		vi.useFakeTimers();
		render(Composer, { props: { ...baseProps(), running: true } });
		const field = screen.getByRole("textbox", { name: "Message" });
		await fireEvent.keyDown(field, { key: "Escape" });
		expect(document.getElementById("composer-help")?.textContent).toBe(
			"Esc again to interrupt",
		);
		await vi.advanceTimersByTimeAsync(2000);
		await tick();
		expect(document.getElementById("composer-help")?.textContent).toContain(
			"Enter to steer",
		);
	});

	test("one Esc interrupts at once during a rate-limit wait", async () => {
		const onInterrupt = vi.fn();
		render(Composer, {
			props: { ...baseProps(), running: true, retrying: true, onInterrupt },
		});
		await fireEvent.keyDown(screen.getByRole("textbox", { name: "Message" }), {
			key: "Escape",
		});
		expect(onInterrupt).toHaveBeenCalledTimes(1);
	});

	test("Esc while idle does nothing", async () => {
		const onInterrupt = vi.fn();
		render(Composer, { props: { ...baseProps(), onInterrupt } });
		await fireEvent.keyDown(screen.getByRole("textbox", { name: "Message" }), {
			key: "Escape",
		});
		expect(onInterrupt).not.toHaveBeenCalled();
		expect(document.getElementById("composer-help")?.textContent).toContain(
			"Enter to send",
		);
	});
});

describe("command palette", () => {
	afterEach(cleanup);

	test("Enter after a command's space keeps the argument and opens no panel", async () => {
		const onOpenPanel = vi.fn();
		render(Composer, { props: { ...baseProps(), onOpenPanel } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "/model foo" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onOpenPanel).not.toHaveBeenCalled();
		expect(field.value).toBe("/model foo");
	});

	test("Escape closes the command list and keeps the message field", async () => {
		render(Composer, { props: baseProps() });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "/mo" } });
		expect(screen.getByRole("listbox", { name: "Commands" })).toBeTruthy();
		await fireEvent.keyDown(field, { key: "Escape" });
		expect(screen.queryByRole("listbox", { name: "Commands" })).toBeNull();
		expect(field.value).toBe("/mo");
	});

	test("Enter with an unknown command does not send the message", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "/zzz" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).not.toHaveBeenCalled();
		expect(
			screen.getByText(
				"No command named /zzz. Start with // to send a message that begins with /.",
			),
		).toBeTruthy();
	});

	test("Tab opens the highlighted command", async () => {
		const onOpenPanel = vi.fn();
		render(Composer, { props: { ...baseProps(), onOpenPanel } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "/mo" } });
		await fireEvent.keyDown(field, { key: "Tab" });
		expect(onOpenPanel).toHaveBeenCalledWith("model");
	});

	test("recalling a command reopens the command list after Escape", async () => {
		render(Composer, { props: { ...baseProps(), history: ["/model"] } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "/mo" } });
		await fireEvent.keyDown(field, { key: "Escape" });
		field.setSelectionRange(0, 0);
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(field.value).toBe("/model");
		expect(screen.getByRole("listbox", { name: "Commands" })).toBeTruthy();
	});

	test("editing a follow-up command reopens the command list after Escape", async () => {
		render(Composer, {
			props: {
				...baseProps(),
				followups: ["/model"],
				onEditFollowup: vi.fn(() => "/model"),
			},
		});
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "/mo" } });
		await fireEvent.keyDown(field, { key: "Escape" });
		await fireEvent.click(screen.getByRole("button", { name: "Edit" }));
		expect(field.value).toBe("/model");
		expect(screen.getByRole("listbox", { name: "Commands" })).toBeTruthy();
	});
});

describe("file mention", () => {
	afterEach(cleanup);

	test("@ opens file completion and Enter inserts the path", async () => {
		const onSend = vi.fn();
		const onFileSearch = vi.fn(() =>
			Promise.resolve([
				{ fileName: "model.go", path: "pkg/model.go", root: "/work" },
			]),
		);
		render(Composer, { props: { ...baseProps(), onSend, onFileSearch } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "@mod" } });
		await waitFor(() => {
			expect(
				screen.getByRole("listbox", { name: "Files" }).textContent,
			).toContain("pkg/model.go");
		});
		expect(onFileSearch).toHaveBeenCalledWith("mod");
		await fireEvent.keyDown(field, { key: "Enter" });
		await waitFor(() => {
			expect(field.value).toBe("@pkg/model.go ");
		});
		expect(onSend).not.toHaveBeenCalled();
	});

	test("Enter after the query changes inserts nothing and sends nothing until the new search answers", async () => {
		const onSend = vi.fn();
		const onFileSearch = vi.fn((query: string) =>
			query === "mod"
				? Promise.resolve([
						{ fileName: "model.go", path: "pkg/model.go", root: "/work" },
					])
				: new Promise<never>(() => undefined),
		);
		render(Composer, { props: { ...baseProps(), onSend, onFileSearch } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "@mod" } });
		await waitFor(() => {
			expect(
				screen.getByRole("listbox", { name: "Files" }).textContent,
			).toContain("pkg/model.go");
		});
		await fireEvent.input(field, { target: { value: "@modx" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(field.value).toBe("@modx");
		expect(onSend).not.toHaveBeenCalled();
	});

	test("moving the caret inside the same mention does not search again", async () => {
		vi.useFakeTimers();
		try {
			const onFileSearch = vi.fn(() =>
				Promise.resolve([
					{ fileName: "model.go", path: "pkg/model.go", root: "/work" },
				]),
			);
			render(Composer, { props: { ...baseProps(), onFileSearch } });
			const field = screen.getByRole("textbox", {
				name: "Message",
			}) as HTMLTextAreaElement;
			await fireEvent.input(field, { target: { value: "@mod" } });
			await vi.advanceTimersByTimeAsync(200);
			await tick();
			expect(onFileSearch).toHaveBeenCalledTimes(1);
			field.setSelectionRange(2, 2);
			await fireEvent.keyUp(field);
			await vi.advanceTimersByTimeAsync(200);
			expect(onFileSearch).toHaveBeenCalledTimes(1);
		} finally {
			vi.useRealTimers();
		}
	});
});
function pastedText(text: string) {
	return {
		getData: (type: string) => (type === "text/plain" ? text : ""),
		files: [],
	};
}

function pastedFiles(files: readonly File[]) {
	return { getData: () => "", files };
}

describe("paste chips and image paste", () => {
	afterEach(cleanup);

	test("a long paste becomes a chip and is sent after the typed text", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "please summarise" } });
		const pasted = Array.from(
			{ length: 12 },
			(_, index) => `line ${index + 1}`,
		).join("\n");
		await fireEvent.paste(field, { clipboardData: pastedText(pasted) });
		expect(screen.getByText("Pasted text · 12 lines")).toBeTruthy();
		expect(field.value).toBe("please summarise");
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenCalledWith("please summarise", {
			texts: [pasted],
			images: [],
		});
		expect(screen.queryByText("Pasted text · 12 lines")).toBeNull();
	});

	test("a pasted image is sent as an image input", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		const shot = new File(
			[new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])],
			"shot.png",
			{
				type: "image/png",
			},
		);
		await fireEvent.paste(field, { clipboardData: pastedFiles([shot]) });
		await waitFor(() => {
			expect(screen.getByText("Image · shot.png · 8 B")).toBeTruthy();
		});
		await fireEvent.input(field, { target: { value: "what is this" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenCalledTimes(1);
		const [text, attachments] = onSend.mock.calls[0] as [
			string,
			{ images: { name: string; dataUrl: string }[] },
		];
		expect(text).toBe("what is this");
		expect(
			attachments.images.map((image) => [image.name, image.dataUrl]),
		).toEqual([["shot.png", "data:image/png;base64,iVBORw0KGgo="]]);
	});

	test("an oversized message is kept with an error", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		const large = () =>
			new File([new Uint8Array(5 * 1024 * 1024)], "big.png", {
				type: "image/png",
			});
		await fireEvent.paste(field, {
			clipboardData: pastedFiles([large(), large(), large()]),
		});
		await waitFor(() => {
			expect(screen.getAllByText(/Image · big\.png/)).toHaveLength(3);
		});
		await fireEvent.input(field, { target: { value: "three screenshots" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).not.toHaveBeenCalled();
		expect(field.value).toBe("three screenshots");
		expect(document.getElementById("composer-error")?.textContent).toBe(
			"The message and images are too large to send together. Remove an image.",
		);
	}, 60000);
	test("pasting eleven images at once keeps ten chips and the limit notice", async () => {
		const reads = vi.spyOn(FileReader.prototype, "readAsDataURL");
		render(Composer, { props: baseProps() });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		const files = Array.from({ length: 11 }, (_, index) =>
			pngFile(`shot${index + 1}.png`),
		);
		await fireEvent.paste(field, { clipboardData: pastedFiles(files) });
		await Promise.all((reads.mock.contexts as FileReader[]).map(waitForRead));
		expect(reads.mock.calls).toHaveLength(10);
		await waitFor(() => {
			expect(screen.getAllByText(/^Image · shot/)).toHaveLength(10);
		});
		expect(document.getElementById("composer-error")?.textContent).toBe(
			"At most 10 images per message.",
		);
		reads.mockRestore();
	});

	test("nine lines with a trailing newline stay in the message field", async () => {
		render(Composer, { props: baseProps() });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		const nine = `${Array.from({ length: 9 }, (_, index) => `line ${index + 1}`).join("\n")}\n`;
		await fireEvent.paste(field, { clipboardData: pastedText(nine) });
		expect(screen.queryByText(/^Pasted text/)).toBeNull();
	});

	test("ten lines with a trailing newline become a ten-line chip", async () => {
		render(Composer, { props: baseProps() });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		const ten = `${Array.from({ length: 10 }, (_, index) => `line ${index + 1}`).join("\n")}\n`;
		await fireEvent.paste(field, { clipboardData: pastedText(ten) });
		expect(screen.getByText("Pasted text · 10 lines")).toBeTruthy();
	});

	test("ten lines without a trailing newline become a ten-line chip", async () => {
		render(Composer, { props: baseProps() });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		const ten = Array.from(
			{ length: 10 },
			(_, index) => `line ${index + 1}`,
		).join("\n");
		await fireEvent.paste(field, { clipboardData: pastedText(ten) });
		expect(screen.getByText("Pasted text · 10 lines")).toBeTruthy();
	});
});

function pngFile(name: string): File {
	return new File([new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])], name, {
		type: "image/png",
	});
}

function waitForRead(reader: FileReader): Promise<void> {
	if (reader.readyState === FileReader.DONE) return Promise.resolve();
	const { promise, resolve } = Promise.withResolvers<void>();
	reader.addEventListener("loadend", () => resolve());
	return promise;
}
