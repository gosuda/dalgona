import type { JsonValue } from "./types";

export function turnSteerParams(
	threadId: string,
	expectedTurnId: string,
	text: string,
): JsonValue {
	if (threadId === "") {
		throw new Error("turnSteerParams requires a non-empty threadId");
	}
	if (expectedTurnId === "") {
		throw new Error("turnSteerParams requires a non-empty expectedTurnId");
	}
	return {
		threadId,
		expectedTurnId,
		input: [{ type: "text", text, text_elements: [] }],
	};
}
