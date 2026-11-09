import { describe, expect, it } from "vitest";
import { turnSteerParams } from "./requests.js";
import type { JsonValue } from "./types.js";
import { parseJsonValue } from "./validation.js";

interface SteerParams {
	readonly threadId: JsonValue;
	readonly expectedTurnId: JsonValue;
	readonly input: JsonValue;
}

function requiredField(value: object, key: string): JsonValue {
	const field = parseJsonValue(Reflect.get(value, key));
	if (field === undefined) {
		throw new Error(`steer params missing ${key}`);
	}
	return field;
}

function asParams(value: JsonValue): SteerParams {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		throw new Error("steer params must be an object");
	}
	return {
		threadId: requiredField(value, "threadId"),
		expectedTurnId: requiredField(value, "expectedTurnId"),
		input: requiredField(value, "input"),
	};
}

describe("turnSteerParams", () => {
	it("carries the Codex v2 precondition fields", () => {
		const params = asParams(turnSteerParams("thread-1", "turn-1", "continue"));
		expect(params.threadId).toBe("thread-1");
		expect(params.expectedTurnId).toBe("turn-1");
		expect(params.input).toEqual([
			{ type: "text", text: "continue", text_elements: [] },
		]);
	});

	it("rejects empty thread and turn ids", () => {
		expect(() => turnSteerParams("", "turn-1", "continue")).toThrow();
		expect(() => turnSteerParams("thread-1", "", "continue")).toThrow();
	});
});
