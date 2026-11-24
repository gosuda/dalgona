import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import {
	answerError,
	correctedQuestion,
	correctionText,
	parseQuestionRequest,
	questionResponse,
	resolutionNotice,
} from "./question";
import type { JsonValue, QuestionRequest } from "./types";

const corpus = JSON.parse(
	readFileSync(
		fileURLToPath(
			new URL("../../../docs/dal/spec/input-text-cases.json", import.meta.url),
		),
		"utf8",
	),
) as { vectors: { id: string; text: string; policy: string }[] };

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

	test("orders labels then the typed text exactly as typed", () => {
		const request = requestOf(vendored);
		const typed = `  ${"x".repeat(2100)}  `;
		expect(
			questionResponse(request, [
				{ picked: ["Postgres", "SQLite"], other: typed },
				{ picked: [], other: "\u3000 " },
			]),
		).toEqual({
			answers: {
				q1: { answers: ["SQLite", "Postgres", typed] },
				q2: { answers: ["\u3000 "] },
			},
		});
	});

	test("sends an empty list for an unanswered question", () => {
		const request = requestOf(vendored);
		expect(questionResponse(request, [])).toEqual({
			answers: { q1: { answers: [] }, q2: { answers: [] } },
		});
	});

	test.each(corpus.vectors.filter((v) => v.policy === "accept"))(
		"$id reaches the response unchanged",
		(v) => {
			const request = requestOf(vendored);
			const response = questionResponse(request, [
				{ picked: [], other: "" },
				{ picked: [], other: v.text },
			]);
			expect(response).toEqual({
				answers: { q1: { answers: [] }, q2: { answers: [v.text] } },
			});
			expect(answerError(v.text)).toBeNull();
		},
	);

	test("maps each resolution to its notice", () => {
		expect(resolutionNotice("answered", false)).toBe(
			"Question answered in another window",
		);
		expect(resolutionNotice("cancelled", false)).toBe(
			"Question dismissed in another window",
		);
		expect(resolutionNotice("interrupted", false)).toBe(
			"Question closed: the turn was interrupted",
		);
		expect(resolutionNotice("declined", false)).toBe(
			"Question closed: the agent continues without an answer",
		);
		expect(resolutionNotice("timedOut", false)).toBe("Question timed out");
		expect(resolutionNotice(null, false)).toBeNull();
	});

	test("does not name a window after this window submitted", () => {
		expect(resolutionNotice("answered", true)).toBe("Question answered");
		expect(resolutionNotice("timedOut", true)).toBe("Question timed out");
	});
});

describe("answer text", () => {
	test("counts the quota in Unicode scalars", () => {
		expect(answerError("a".repeat(2000))).toBeNull();
		expect(answerError("\u{1F600}".repeat(2000))).toBeNull();
		expect(answerError("a".repeat(2001))).toBe(
			"Shorten this answer to 2000 characters or fewer.",
		);
		expect(answerError(`e${"\u0301".repeat(2000)}`)).toBe(
			"Shorten this answer to 2000 characters or fewer.",
		);
	});

	test("refuses NUL and a lone surrogate", () => {
		expect(answerError("keep\u0000this")).toBe(
			"Remove the NUL character from this answer.",
		);
		expect(answerError("bad\ud800")).toBe(
			"Retype this answer: it holds an incomplete character.",
		);
	});

	test("keeps whitespace-only text as an answer", () => {
		expect(answerError(" \u3000\t")).toBeNull();
	});

	test("reads the question from a correction field", () => {
		expect(correctedQuestion("answers.q1.answers[0]")).toBe("q1");
		expect(correctedQuestion("answers.a.answers[1].answers[12]")).toBe(
			"a.answers[1]",
		);
		expect(correctedQuestion("answers.q1")).toBeNull();
		expect(correctedQuestion("input")).toBeNull();
	});

	test("explains a correction by its reason", () => {
		expect(correctionText("text_too_large", "invalid text")).toBe(
			"Shorten this answer to 2000 characters or fewer.",
		);
		expect(correctionText("something_new", "invalid text: q1")).toBe(
			"invalid text: q1",
		);
	});
});
