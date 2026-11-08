import { describe, expect, it } from "vitest";
import { turnSteerParams } from "./requests.js";

describe("turnSteerParams", () => {
	it("uses the active turn precondition required by Codex v2", () => {
		expect(turnSteerParams("thread-1", "turn-1", "continue")).toEqual({
			threadId: "thread-1",
			expectedTurnId: "turn-1",
			input: [{ type: "text", text: "continue", text_elements: [] }],
		});
	});
});
