import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, test } from "vitest";
import Transcript from "#lib/components/Transcript.svelte";
import type { TranscriptItem } from "#lib/types.ts";

function props(items: readonly TranscriptItem[]) {
	return {
		threadId: "thread-1",
		items,
		turnStatus: "inProgress" as const,
		error: null,
		plan: null,
		connected: true,
		keywords: [],
		loading: false,
		awaitingApproval: new Set<string>(),
	};
}

function reply(id: string, text: string, complete: boolean): TranscriptItem {
	return { id, kind: "agentMessage", text, complete };
}

afterEach(cleanup);

describe("transcript live regions", () => {
	test("streamed text is outside every live region", () => {
		const { container } = render(Transcript, {
			props: props([reply("m1", "partial answer", false)]),
		});
		const message = container.querySelector(".agent-message .prose");
		expect(message).not.toBeNull();
		expect(
			message?.closest("[role=log],[role=status],[role=alert],[aria-live]"),
		).toBeNull();
	});
});
