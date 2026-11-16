import { describe, expect, test } from "vitest";
import { renderDiagram } from "./diagrams";

describe("renderDiagram svg sanitization", () => {
	test("drops style elements and style attributes", async () => {
		const svg = await renderDiagram(
			"svg",
			`<svg><style>body{display:none}</style><rect width="1" height="1" style="position:fixed;top:0"/></svg>`,
		);
		expect(svg.includes("<style")).toBe(false);
		expect(svg.includes("style=")).toBe(false);
	});

	test("drops elements that load remote resources", async () => {
		const svg = await renderDiagram(
			"svg",
			`<svg><image href="https://evil.test/x.png"/><feImage href="https://evil.test/y.png"/><rect/></svg>`,
		);
		expect(svg.toLowerCase().includes("<image")).toBe(false);
		expect(svg.toLowerCase().includes("<feimage")).toBe(false);
		expect(svg.includes("<rect")).toBe(true);
	});
});
