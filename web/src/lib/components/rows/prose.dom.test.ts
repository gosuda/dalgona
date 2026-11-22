import "../../../app.css";
import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, test } from "vitest";
import AdvisorNote from "#lib/components/rows/AdvisorNote.svelte";
import MessageRow from "#lib/components/rows/MessageRow.svelte";
import PlanPanel from "#lib/components/rows/PlanPanel.svelte";
import TextRow from "#lib/components/rows/TextRow.svelte";
import type { TranscriptItem } from "#lib/types.ts";

const token = "가".repeat(200);

function row(item: TranscriptItem) {
	return { item, running: false, awaiting: new Set<string>() };
}

function expectProse(element: Element | null) {
	expect(element).not.toBeNull();
	const style = getComputedStyle(element as Element);
	expect(style.wordBreak).toBe("keep-all");
	expect(style.overflowWrap).toBe("anywhere");
}

afterEach(cleanup);

describe("prose rows use keep-all and overflow-wrap anywhere", () => {
	test("a user message", () => {
		const { container } = render(MessageRow, {
			props: row({
				id: "u1",
				kind: "userMessage",
				text: token,
				turnId: "turn-1",
				keywords: [],
			}),
		});
		expectProse(container.querySelector(".user-message .prose"));
	});

	test("an agent message", () => {
		const { container } = render(MessageRow, {
			props: row({
				id: "a1",
				kind: "agentMessage",
				text: token,
				complete: true,
			}),
		});
		expectProse(container.querySelector(".agent-message .prose"));
	});

	test("a plan row", () => {
		const { container } = render(TextRow, {
			props: row({ id: "p1", kind: "plan", text: token }),
		});
		expectProse(container.querySelector(".plan p"));
	});

	test("an advisor note", () => {
		const { container } = render(AdvisorNote, {
			props: row({
				id: "n1",
				kind: "advisorNote",
				advisor: "advisor",
				severity: "info",
				note: token,
			}),
		});
		expectProse(container.querySelector(".advisor-note"));
	});

	test("a plan step and the plan explanation", () => {
		const { container } = render(PlanPanel, {
			props: {
				plan: {
					turnId: "turn-1",
					explanation: token,
					steps: [{ step: token, status: "inProgress" }],
				},
			},
		});
		expectProse(container.querySelector(".turn-plan p"));
		expectProse(container.querySelector(".turn-plan li"));
	});
});
