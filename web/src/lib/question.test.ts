import { describe, expect, test } from "vitest";
import {
	MAX_ANSWER,
	parseQuestionRequest,
	questionResponse,
	resolutionNotice,
} from "./question";
import type { JsonValue, QuestionRequest } from "./types";

const vendored: JsonValue = {
	threadId: "thread-1",
	turnId: "turn-1",
	itemId: "call-q",
	isBlocking: true,
	autoResolutionMs: null,
	questions: [
		{
			id: "q1",
			header: "Database",
			question: "Which database should the service use?",
			isOther: true,
			isSecret: false,
			multiSelect: false,
			options: [
				{ label: "SQLite", description: "One file, no server to run." },
				{
					label: "Postgres",
					description: "A server with replication.",
					preview: "services:\n  db:\n    image: postgres:17",
				},
			],
		},
		{
			id: "q2",
			header: "Name",
			question: "What should the service be called?",
			isOther: false,
			isSecret: false,
			options: null,
		},
	],
};

function requestOf(params: JsonValue): QuestionRequest {
	const request = parseQuestionRequest("req-1", params);
	if (request === null) throw new Error("the request did not parse");
	return request;
}

describe("question request", () => {
	test("parses a vendored request", () => {
		expect(parseQuestionRequest("req-k3", vendored)).toEqual({
			id: "req-k3",
			method: "item/tool/requestUserInput",
			threadId: "thread-1",
			turnId: "turn-1",
			itemId: "call-q",
			questions: [
				{
					id: "q1",
					header: "Database",
					question: "Which database should the service use?",
					isOther: true,
					multiSelect: false,
					options: [
						{
							label: "SQLite",
							description: "One file, no server to run.",
							preview: null,
						},
						{
							label: "Postgres",
							description: "A server with replication.",
							preview: "services:\n  db:\n    image: postgres:17",
						},
					],
				},
				{
					id: "q2",
					header: "Name",
					question: "What should the service be called?",
					isOther: false,
					multiSelect: false,
					options: [],
				},
			],
		});
	});

	test("rejects a request without questions", () => {
		expect(
			parseQuestionRequest(1, {
				threadId: "thread-1",
				turnId: "turn-1",
				itemId: "call-q",
			}),
		).toBeNull();
		expect(
			parseQuestionRequest(1, {
				threadId: "thread-1",
				turnId: "turn-1",
				itemId: "call-q",
				questions: [],
			}),
		).toBeNull();
	});

	test("treats null options as free text", () => {
		const request = requestOf(vendored);
		expect(request.questions[1]?.options).toEqual([]);
	});

	test("orders labels then other text and caps it", () => {
		const request = requestOf(vendored);
		const typed = `  ${"x".repeat(2100)}  `;
		expect(
			questionResponse(request, [
				{ picked: ["Postgres", "SQLite"], other: typed },
				{ picked: [], other: "" },
			]),
		).toEqual({
			answers: {
				q1: { answers: ["SQLite", "Postgres", "x".repeat(2000)] },
				q2: { answers: [] },
			},
		});
		expect(MAX_ANSWER).toBe(2000);
	});

	test("maps each resolution to its notice", () => {
		expect(resolutionNotice("answered")).toBe(
			"Question answered in another window",
		);
		expect(resolutionNotice("cancelled")).toBe(
			"Question dismissed in another window",
		);
		expect(resolutionNotice("interrupted")).toBe(
			"Question closed: the turn was interrupted",
		);
		expect(resolutionNotice("declined")).toBe(
			"Question closed: the agent continues without an answer",
		);
		expect(resolutionNotice(null)).toBeNull();
	});
});
