import { waitFor } from "@testing-library/dom";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import Composer from "#lib/components/Composer.svelte";
import { appliedRule } from "#lib/css-rule.ts";
import type { FileMatch } from "#lib/types.ts";

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
		const interrupt = screen.getByRole("button", { name: "Interrupt" });
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

describe("running actions", () => {
	afterEach(cleanup);

	test("names the interrupt button Interrupt and calls onInterrupt", async () => {
		const onInterrupt = vi.fn();
		render(Composer, { props: { ...baseProps(), running: true, onInterrupt } });
		await fireEvent.click(screen.getByRole("button", { name: "Interrupt" }));
		expect(onInterrupt).toHaveBeenCalledTimes(1);
	});

	test("offers no Interrupt button while idle", () => {
		render(Composer, { props: baseProps() });
		expect(screen.queryByRole("button", { name: "Interrupt" })).toBeNull();
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
		const field = screen.getByRole("textbox", { name: "Message" });
		const before = document.getElementById("composer-help")?.textContent;
		await fireEvent.keyDown(field, { key: "Escape" });
		await fireEvent.keyDown(field, { key: "Escape" });
		expect(onInterrupt).not.toHaveBeenCalled();
		expect(document.getElementById("composer-help")?.textContent).toBe(before);
	});
});

describe("send point", () => {
	afterEach(cleanup);

	test("Send holds the point with a thread open and idle", () => {
		render(Composer, { props: { ...baseProps(), threadOpen: true } });
		expect(screen.getByRole("button", { name: "Send" }).className).toBe(
			"point",
		);
	});

	test("Send is quiet with no thread open", () => {
		render(Composer, { props: { ...baseProps(), threadOpen: false } });
		const composer = document.querySelector(".composer");
		const send = screen.getByRole("button", { name: "Send" });
		expect(send.className).toBe("quiet");
		expect(composer?.querySelector(".point")).toBeNull();
	});

	test("a running turn and a shown decision both keep Send quiet", () => {
		render(Composer, {
			props: { ...baseProps(), threadOpen: true, running: true },
		});
		expect(screen.getByRole("button", { name: "Steer" }).className).toBe(
			"quiet",
		);
		cleanup();
		render(Composer, {
			props: { ...baseProps(), threadOpen: true, deciding: true },
		});
		expect(screen.getByRole("button", { name: "Send" }).className).toBe(
			"quiet",
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

	test("file completion wraps within the shown rows", async () => {
		const twentieth: FileMatch = {
			fileName: "unit19.go",
			path: "pkg/unit19.go",
			root: "/work",
		};
		const matches: readonly FileMatch[] = [
			...Array.from({ length: 19 }, (_, index) => ({
				fileName: `unit${index}.go`,
				path: `pkg/unit${index}.go`,
				root: "/work",
			})),
			twentieth,
			...Array.from({ length: 30 }, (_, index) => ({
				fileName: `unit${index + 20}.go`,
				path: `pkg/unit${index + 20}.go`,
				root: "/work",
			})),
		];
		const onFileSearch = vi.fn(() => Promise.resolve(matches));
		render(Composer, { props: { ...baseProps(), onFileSearch } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "@unit" } });
		await waitFor(() => {
			expect(screen.getAllByRole("option")).toHaveLength(20);
		});
		const activeId = () => field.getAttribute("aria-activedescendant");
		expect(activeId()).toBe("file-completion-0");
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(activeId()).toBe("file-completion-19");
		expect(
			document.getElementById("file-completion-19")?.textContent,
		).toContain(twentieth.path);
		expect(
			document
				.getElementById("file-completion-19")
				?.getAttribute("aria-selected"),
		).toBe("true");
		await fireEvent.keyDown(field, { key: "ArrowDown" });
		expect(activeId()).toBe("file-completion-0");
		for (let index = 1; index < 20; index += 1) {
			await fireEvent.keyDown(field, { key: "ArrowDown" });
			expect(activeId()).toBe(`file-completion-${index}`);
		}
		await fireEvent.keyDown(field, { key: "ArrowDown" });
		expect(activeId()).toBe("file-completion-0");
		await fireEvent.keyDown(field, { key: "ArrowUp" });
		expect(activeId()).toBe("file-completion-19");
		await fireEvent.keyDown(field, { key: "Enter" });
		await waitFor(() => {
			expect(field.value).toBe(`@${twentieth.path} `);
		});
		cleanup();

		render(Composer, { props: { ...baseProps(), onFileSearch } });
		const tabField = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(tabField, { target: { value: "@unit" } });
		await waitFor(() => {
			expect(screen.getAllByRole("option")).toHaveLength(20);
		});
		await fireEvent.keyDown(tabField, { key: "ArrowUp" });
		await fireEvent.keyDown(tabField, { key: "Tab" });
		await waitFor(() => {
			expect(tabField.value).toBe(`@${twentieth.path} `);
		});
		cleanup();

		const onThree = vi.fn(() => Promise.resolve(matches.slice(0, 3)));
		render(Composer, { props: { ...baseProps(), onFileSearch: onThree } });
		const threeField = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(threeField, { target: { value: "@unit" } });
		await waitFor(() => {
			expect(screen.getAllByRole("option")).toHaveLength(3);
		});
		await fireEvent.keyDown(threeField, { key: "ArrowUp" });
		expect(threeField.getAttribute("aria-activedescendant")).toBe(
			"file-completion-2",
		);
		cleanup();

		vi.useFakeTimers();
		try {
			const onSend = vi.fn();
			const onEmpty = vi.fn(() => Promise.resolve<readonly FileMatch[]>([]));
			render(Composer, {
				props: { ...baseProps(), onSend, onFileSearch: onEmpty },
			});
			const emptyField = screen.getByRole("textbox", {
				name: "Message",
			}) as HTMLTextAreaElement;
			await fireEvent.input(emptyField, { target: { value: "@none" } });
			await vi.advanceTimersByTimeAsync(200);
			await tick();
			expect(onEmpty).toHaveBeenCalledWith("none");
			expect(screen.queryByRole("listbox", { name: "Files" })).toBeNull();
			await fireEvent.keyDown(emptyField, { key: "ArrowUp" });
			expect(emptyField.value).toBe("@none");
			await fireEvent.keyDown(emptyField, { key: "Enter" });
			expect(onSend).toHaveBeenCalledTimes(1);
			expect(onSend.mock.calls[0]?.[0]).toBe("@none");
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

describe("shell mode", () => {
	afterEach(cleanup);

	test("a leading ! labels the field and shows the shell help line", async () => {
		render(Composer, { props: { ...baseProps(), cwd: "/work" } });
		const field = screen.getByRole("textbox", { name: "Message" });
		await fireEvent.input(field, { target: { value: "!ls" } });
		expect(screen.getByRole("textbox", { name: "Shell command" })).toBe(field);
		expect(document.getElementById("composer-help")?.textContent?.trim()).toBe(
			"Enter runs the command in /work. It is not sent to the agent.",
		);
	});

	test("Enter runs the command and shows its output as a chip", async () => {
		const onShell = vi.fn(() =>
			Promise.resolve({
				n: 1,
				exitCode: 0,
				lines: 1,
				text: "Output of `ls` (exit 0):\n```\na.txt\n```",
			}),
		);
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onShell, onSend } });
		const field = screen.getByRole("textbox", { name: "Message" });
		await fireEvent.input(field, { target: { value: "!ls" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onShell).toHaveBeenCalledWith("ls");
		expect(onSend).not.toHaveBeenCalled();
		await waitFor(() => {
			expect(screen.getByText("[Shell #1 exit 0, 1 line]")).toBeTruthy();
		});
	});

	test("a shell line the session cannot run keeps the draft", async () => {
		const onShell = vi.fn(() => null);
		render(Composer, { props: { ...baseProps(), onShell } });
		const field = screen.getByRole("textbox", { name: "Message" });
		await fireEvent.input(field, { target: { value: "!ls" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onShell).toHaveBeenCalledWith("ls");
		expect((field as HTMLTextAreaElement).value).toBe("!ls");
	});

	test("Alt+Enter on a shell line queues nothing and says shell commands run with Enter", async () => {
		const onFollowup = vi.fn();
		render(Composer, { props: { ...baseProps(), running: true, onFollowup } });
		const field = screen.getByRole("textbox");
		await fireEvent.input(field, { target: { value: "!ls" } });
		await fireEvent.keyDown(field, { key: "Enter", altKey: true });
		expect(onFollowup).not.toHaveBeenCalled();
		expect(document.querySelector(".error")?.textContent).toBe(
			"Shell commands run with Enter",
		);
		expect((field as HTMLTextAreaElement).value).toBe("!ls");
	});

	test("an empty shell line is refused and runs nothing", async () => {
		const onShell = vi.fn();
		render(Composer, { props: { ...baseProps(), onShell } });
		const field = screen.getByRole("textbox", { name: "Message" });
		await fireEvent.input(field, { target: { value: "!" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onShell).not.toHaveBeenCalled();
		expect(document.querySelector(".error")?.textContent).toBe(
			"Enter a command after !.",
		);
	});
});

describe("escaped slash", () => {
	afterEach(cleanup);

	test("a double slash sends one slash", async () => {
		const onSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend } });
		const field = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(field, { target: { value: "//help" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenCalledWith("/help", { texts: [], images: [] });
		await fireEvent.input(field, { target: { value: "hello" } });
		await fireEvent.keyDown(field, { key: "Enter" });
		expect(onSend).toHaveBeenLastCalledWith("hello", {
			texts: [],
			images: [],
		});
		cleanup();

		const onSteer = vi.fn();
		const onFollowup = vi.fn();
		render(Composer, {
			props: { ...baseProps(), running: true, onSteer, onFollowup },
		});
		const runningField = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(runningField, { target: { value: "//help" } });
		await fireEvent.keyDown(runningField, { key: "Enter" });
		expect(onSteer).toHaveBeenCalledWith("/help");
		await fireEvent.input(runningField, { target: { value: "hello" } });
		await fireEvent.keyDown(runningField, { key: "Enter" });
		expect(onSteer).toHaveBeenLastCalledWith("hello");
		await fireEvent.input(runningField, { target: { value: "//help" } });
		await fireEvent.keyDown(runningField, { key: "Enter", altKey: true });
		expect(onFollowup).toHaveBeenCalledWith("/help");
		await fireEvent.input(runningField, { target: { value: "hello" } });
		await fireEvent.keyDown(runningField, { key: "Enter", altKey: true });
		expect(onFollowup).toHaveBeenLastCalledWith("hello");
		cleanup();

		const onOpenPanel = vi.fn();
		const idleSend = vi.fn();
		render(Composer, {
			props: { ...baseProps(), onOpenPanel, onSend: idleSend },
		});
		const commandField = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(commandField, { target: { value: "/help" } });
		await fireEvent.keyDown(commandField, { key: "Enter" });
		expect(onOpenPanel).toHaveBeenCalledWith("keys");
		expect(idleSend).not.toHaveBeenCalled();
		cleanup();

		const runningOpenPanel = vi.fn();
		const runningSteer = vi.fn();
		const runningFollowup = vi.fn();
		render(Composer, {
			props: {
				...baseProps(),
				running: true,
				onOpenPanel: runningOpenPanel,
				onSteer: runningSteer,
				onFollowup: runningFollowup,
			},
		});
		const paletteField = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(paletteField, { target: { value: "/help" } });
		await fireEvent.keyDown(paletteField, { key: "Enter" });
		expect(runningOpenPanel).toHaveBeenCalledWith("keys");
		expect(runningSteer).not.toHaveBeenCalled();
		await fireEvent.input(paletteField, { target: { value: "/help" } });
		await fireEvent.keyDown(paletteField, { key: "Enter", altKey: true });
		expect(runningFollowup).not.toHaveBeenCalled();
		expect(paletteField.value).toBe("/help");
		cleanup();

		const onShell = vi.fn(() => null);
		const shellSend = vi.fn();
		const shellSteer = vi.fn();
		render(Composer, {
			props: {
				...baseProps(),
				running: true,
				onShell,
				onSend: shellSend,
				onSteer: shellSteer,
			},
		});
		const shellField = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(shellField, { target: { value: "!ls" } });
		await fireEvent.keyDown(shellField, { key: "Enter" });
		expect(onShell).toHaveBeenCalledWith("ls");
		expect(shellSend).not.toHaveBeenCalled();
		expect(shellSteer).not.toHaveBeenCalled();
		cleanup();

		const emptySend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend: emptySend } });
		const emptyField = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		await fireEvent.input(emptyField, { target: { value: "   " } });
		await fireEvent.keyDown(emptyField, { key: "Enter" });
		expect(emptySend).not.toHaveBeenCalled();
		expect(document.getElementById("composer-error")?.textContent).toBe(
			"Enter a message before sending.",
		);
		expect(document.activeElement).toBe(emptyField);
		cleanup();

		const attachmentSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend: attachmentSend } });
		const attachmentField = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		const pasted = Array.from(
			{ length: 10 },
			(_, index) => `line ${index + 1}`,
		).join("\n");
		await fireEvent.paste(attachmentField, {
			clipboardData: pastedText(pasted),
		});
		expect(screen.getByText("Pasted text · 10 lines")).toBeTruthy();
		await fireEvent.keyDown(attachmentField, { key: "Enter" });
		expect(attachmentSend).toHaveBeenCalledWith("", {
			texts: [pasted],
			images: [],
		});
		expect(screen.queryByText("Pasted text · 10 lines")).toBeNull();
		cleanup();

		const limitSend = vi.fn();
		render(Composer, { props: { ...baseProps(), onSend: limitSend } });
		const limitField = screen.getByRole("textbox", {
			name: "Message",
		}) as HTMLTextAreaElement;
		const large = () =>
			new File([new Uint8Array(5 * 1024 * 1024)], "big.png", {
				type: "image/png",
			});
		await fireEvent.paste(limitField, {
			clipboardData: pastedFiles([large(), large(), large()]),
		});
		await waitFor(() => {
			expect(screen.getAllByText(/Image · big\.png/)).toHaveLength(3);
		});
		await fireEvent.input(limitField, { target: { value: "//help" } });
		await fireEvent.keyDown(limitField, { key: "Enter" });
		expect(limitSend).not.toHaveBeenCalled();
		expect(limitField.value).toBe("//help");
		expect(screen.getAllByText(/Image · big\.png/)).toHaveLength(3);
		expect(document.getElementById("composer-error")?.textContent).toBe(
			"The message and images are too large to send together. Remove an image.",
		);
	}, 60000);
});

describe("composer under a request", () => {
	afterEach(cleanup);

	test("gives way to the request card down to its label and field", () => {
		render(Composer, { props: { ...baseProps(), deciding: true } });
		const rule = appliedRule(".composer.deciding");
		expect(rule.style.getPropertyValue("flex-shrink")).toBe("1");
		expect(rule.style.getPropertyValue("min-height")).toContain(
			"var(--control-height)",
		);
	});
});
