import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import AttachmentChips from "#lib/components/AttachmentChips.svelte";
import CompletionList from "#lib/components/CompletionList.svelte";

afterEach(cleanup);

function escapesIn(element: Element | null | undefined): readonly string[] {
	return [...(element?.querySelectorAll(".escape") ?? [])].map(
		(marker) => marker.getAttribute("data-escape") ?? "",
	);
}

describe("display escapes in chips and completions", () => {
	test("a paste chip and an image name show control escapes and keep their source text", async () => {
		const pasted = "red \x1b[31m\u202eevil\n\tnext \u{1F469}\u200d\u{1F4BB}";
		const name = "photo\u202egnp.exe";
		render(AttachmentChips, {
			props: {
				chips: [
					{ kind: "paste", id: 1, text: pasted, lines: 2 },
					{
						kind: "image",
						id: 2,
						name,
						mime: "image/png",
						size: 10,
						dataUrl: "data:image/png;base64,",
					},
				],
				onRemove: vi.fn(),
			},
		});
		await fireEvent.click(screen.getByRole("button", { name: "Show" }));
		const shown = document.querySelector(".chips pre");
		expect(shown?.textContent).toBe(pasted);
		expect(escapesIn(shown)).toEqual(["U+001B", "U+202E"]);
		const image = [...document.querySelectorAll(".chip .label")].find(
			(label) => label.textContent?.includes(name) === true,
		);
		expect(image?.textContent).toBe(`Image · ${name} · 10 B`);
		expect(escapesIn(image)).toEqual(["U+202E"]);
	});

	test("file completions show control escapes in names and paths", () => {
		render(CompletionList, {
			props: {
				id: "files",
				label: "Files",
				active: 0,
				items: [
					{
						id: "src/a\u202eb.ts",
						primary: "a\u202eb.ts",
						secondary: "src/a\u202eb.ts",
					},
				],
			},
		});
		const option = screen.getByRole("option");
		expect(option.querySelector(".primary")?.textContent).toBe("a\u202eb.ts");
		expect(option.querySelector(".secondary")?.textContent).toBe(
			"src/a\u202eb.ts",
		);
		expect(escapesIn(option)).toEqual(["U+202E", "U+202E"]);
	});
});
