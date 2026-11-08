import type { JsonValue } from "./types";

export function turnSteerParams(
	threadId: string,
	expectedTurnId: string,
	text: string,
): JsonValue {
	return {
		threadId,
		expectedTurnId,
		input: [{ type: "text", text, text_elements: [] }],
	};
}
