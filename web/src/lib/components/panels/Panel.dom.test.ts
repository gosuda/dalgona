import { cleanup, render } from "@testing-library/svelte";
import { createRawSnippet, tick } from "svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import Panel from "#lib/components/panels/Panel.svelte";

afterEach(cleanup);

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
