import { cleanup, render } from "@testing-library/svelte";
import { createRawSnippet, tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import Panel from "#lib/components/panels/Panel.svelte";

afterEach(() => {
	cleanup();
	document.body.replaceChildren();
});

function bodyWith(html: string) {
	return createRawSnippet(() => ({ render: () => html }));
}

describe("panel focus", () => {
	test.each([
		[
			"an input",
			'<input id="first" type="text"><button type="button">Later</button>',
		],
		[
			"a select",
			'<select id="first"><option>a</option></select><button type="button">Later</button>',
		],
		[
			"a textarea",
			'<textarea id="first"></textarea><button type="button">Later</button>',
		],
		[
			"a summary",
			'<details><summary id="first">More</summary></details><button type="button">Later</button>',
		],
		[
			"an element with a tabindex",
			'<div id="first" tabindex="0">Item</div><button type="button">Later</button>',
		],
	])("focuses %s, the first focusable control in the body", async (_, html) => {
		render(Panel, {
			props: {
				title: "Full text",
				open: true,
				onClose: vi.fn(),
				children: bodyWith(html),
			},
		});
		await tick();
		expect(document.activeElement?.id).toBe("first");
	});
});

describe("panel close", () => {
	function field(): HTMLTextAreaElement {
		const prompt = document.createElement("textarea");
		prompt.id = "prompt";
		document.body.append(prompt);
		return prompt;
	}

	async function closed(opener: HTMLElement | null): Promise<void> {
		opener?.focus();
		const view = render(Panel, {
			props: {
				title: "Keys",
				open: true,
				onClose: vi.fn(),
				children: bodyWith('<button type="button">Inside</button>'),
			},
		});
		await tick();
		expect(document.querySelector("dialog[open]")).not.toBeNull();
		await view.rerender({ open: false });
		await tick();
		expect(document.querySelector("dialog[open]")).toBeNull();
	}

	test("returns focus to a connected, enabled opener", async () => {
		const prompt = field();
		const opener = document.createElement("button");
		document.body.append(opener);
		await closed(opener);
		expect(document.activeElement).toBe(opener);
		expect(document.activeElement).not.toBe(prompt);
	});

	test("hands focus to the message field when the page body opened it", async () => {
		const prompt = field();
		(document.activeElement as HTMLElement | null)?.blur();
		expect(document.activeElement).toBe(document.body);
		await closed(null);
		expect(document.activeElement).toBe(prompt);
	});

	test("hands focus to the message field when the opener left the page", async () => {
		const prompt = field();
		const opener = document.createElement("button");
		document.body.append(opener);
		opener.focus();
		const view = render(Panel, {
			props: {
				title: "Keys",
				open: true,
				onClose: vi.fn(),
				children: bodyWith('<button type="button">Inside</button>'),
			},
		});
		await tick();
		opener.remove();
		await view.rerender({ open: false });
		await tick();
		expect(document.activeElement).toBe(prompt);
	});

	test("hands focus to the message field when the opener is disabled", async () => {
		const prompt = field();
		const opener = document.createElement("button");
		document.body.append(opener);
		opener.focus();
		const view = render(Panel, {
			props: {
				title: "Keys",
				open: true,
				onClose: vi.fn(),
				children: bodyWith('<button type="button">Inside</button>'),
			},
		});
		await tick();
		opener.disabled = true;
		await view.rerender({ open: false });
		await tick();
		expect(document.activeElement).toBe(prompt);
	});
});
