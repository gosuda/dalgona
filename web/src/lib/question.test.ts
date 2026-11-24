import { describe, expect, test } from "vitest";
import corpus from "../../../docs/dal/spec/input-text-cases.json";
import {
	answerFault,
	correctionOf,
	MAX_ANSWER,
	parseQuestionRequest,
	parseQuestionWarning,
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

	test("orders labels then the exact typed text", () => {
		const request = requestOf(vendored);
		const typed = `  ${"x".repeat(1996)}  `;
		expect(
			questionResponse(request, [
				{ picked: ["Postgres", "SQLite"], other: typed },
				{ picked: [], other: "" },
			]),
		).toEqual({
			answers: {
				q1: { answers: ["SQLite", "Postgres", typed] },
				q2: { answers: [] },
			},
		});
	});

	test("a whitespace-only answer stays as typed", () => {
		const request = requestOf(vendored);
		expect(
			questionResponse(request, [
				{ picked: [], other: " \u00a0\u3000 " },
				{ picked: [], other: "\n" },
			]),
		).toEqual({
			answers: {
				q1: { answers: [" \u00a0\u3000 "] },
				q2: { answers: ["\n"] },
			},
		});
	});

	test("every accepted corpus vector is a valid answer sent unchanged", () => {
		const request = requestOf(vendored);
		for (const vector of corpus.vectors.filter(
			(entry) => entry.policy === "accept",
		)) {
			expect(answerFault(vector.text), vector.id).toBeNull();
			expect(
				questionResponse(request, [
					{ picked: [], other: vector.text },
					{ picked: [], other: "" },
				]),
				vector.id,
			).toEqual({
				answers: { q1: { answers: [vector.text] }, q2: { answers: [] } },
			});
		}
	});
});

describe("answer quota", () => {
	test("allows 2000 scalars and refuses 2001 whole", () => {
		expect(MAX_ANSWER).toBe(2000);
		expect(answerFault("x".repeat(2000))).toBeNull();
		expect(answerFault("x".repeat(2001))).toBe("text_too_large");
	});

	test("counts each supplementary scalar once, not as two UTF-16 units", () => {
		const emoji = "\u{1F600}".repeat(2000);
		expect(emoji.length).toBe(4000);
		expect(answerFault(emoji)).toBeNull();
		expect(answerFault(`${emoji}x`)).toBe("text_too_large");
	});

	test("refuses one cluster of 2001 scalars without splitting it", () => {
		expect(answerFault(`e${"\u0301".repeat(2000)}`)).toBe("text_too_large");
		expect(answerFault(`e${"\u0301".repeat(100000)}`)).toBe("text_too_large");
	});

	test("refuses NUL and lone surrogates", () => {
		expect(answerFault("a\0b")).toBe("disallowed_control");
		expect(answerFault("a\ud800")).toBe("invalid_encoding");
		expect(answerFault("\udc00")).toBe("invalid_encoding");
	});
});

describe("required question fields", () => {
	function withQuestion(question: Record<string, JsonValue>): JsonValue {
		return {
			threadId: "thread-1",
			turnId: "turn-1",
			itemId: "call-q",
			questions: [
				{
					id: "q1",
					header: "Database",
					question: "Which database?",
					isOther: true,
					options: [{ label: "SQLite", description: "One file." }],
					...question,
				},
			],
		};
	}

	test("rejects blank or formatting-only required fields", () => {
		const blanks = [
			{ header: " \u3000" },
			{ question: "\u200d\ufe0f" },
			{ header: "\x1b[2J\x1b]0;title\x07" },
			{ question: "\u202e\u2066" },
			{ options: [{ label: "\u00ad", description: "One file." }] },
			{ options: [{ label: "SQLite", description: "\t\n" }] },
		];
		for (const fields of blanks) {
			expect(
				parseQuestionRequest("req-1", withQuestion(fields)),
				JSON.stringify(fields),
			).toBeNull();
		}
	});

	test("rejects NUL and lone surrogates in model text", () => {
		const invalid = [
			{ header: "Data\0base" },
			{ question: "Which\ud800?" },
			{
				options: [
					{ label: "SQLite", description: "One file.", preview: "\udc00" },
				],
			},
		];
		for (const fields of invalid) {
			expect(
				parseQuestionRequest("req-1", withQuestion(fields)),
				JSON.stringify(fields),
			).toBeNull();
		}
	});

	test("keeps accepted required text exactly, padding included", () => {
		const request = parseQuestionRequest(
			"req-1",
			withQuestion({
				header: "  Database\u200d ",
				question: "\u3000Which database?\n",
				options: [{ label: " SQLite\r\n", description: "\x1b[1mOne file." }],
			}),
		);
		const question = request?.questions[0];
		expect(question?.header).toBe("  Database\u200d ");
		expect(question?.question).toBe("\u3000Which database?\n");
		expect(question?.options[0]?.label).toBe(" SQLite\r\n");
		expect(question?.options[0]?.description).toBe("\x1b[1mOne file.");
	});
});

describe("resolution notices", () => {
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
		expect(resolutionNotice("timedOut")).toBe("Question timed out");
		expect(resolutionNotice(null)).toBeNull();
	});
});

describe("question corrections", () => {
	const warning = {
		threadId: "thread-1",
		message: "invalid text: answers.q.1.answers[2]: text_too_large",
		requestId: "req-1",
		reason: "text_too_large",
		field: "answers.q.1.answers[2]",
	};

	test("parses a warning that names a question request", () => {
		expect(parseQuestionWarning(warning)).toEqual(warning);
		expect(
			parseQuestionWarning({ threadId: null, message: "disk" }),
		).toBeNull();
		expect(
			parseQuestionWarning({ ...warning, requestId: undefined }),
		).toBeNull();
		expect(parseQuestionWarning({ ...warning, field: 3 })).toBeNull();
	});

	test("reads the question and answer index from the field path", () => {
		expect(correctionOf(warning)).toEqual({
			questionId: "q.1",
			answerIndex: 2,
			reason: "text_too_large",
		});
		expect(correctionOf({ ...warning, field: "answers.q1" })).toEqual({
			questionId: null,
			answerIndex: null,
			reason: "text_too_large",
		});
	});
});
