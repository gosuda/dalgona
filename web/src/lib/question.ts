import { isBlank, scalarCount, type TextFault, textFault } from "./inputtext";
import type {
	JsonValue,
	QuestionCorrection,
	QuestionItem,
	QuestionOption,
	QuestionRequest,
	QuestionSelection,
	QuestionWarning,
	RequestId,
	ServerRequest,
} from "./types";
import { QUESTION_METHOD } from "./types";

export const MAX_ANSWER = 2000;

export type AnswerFault = TextFault | "text_too_large";

interface Members {
	readonly [key: string]: unknown;
}

const resolutionNotices: Readonly<Record<string, string>> = {
	answered: "Question answered in another window",
	cancelled: "Question dismissed in another window",
	interrupted: "Question closed: the turn was interrupted",
	declined: "Question closed: the agent continues without an answer",
	timedOut: "Question timed out",
};

const correctionNotices: Readonly<Record<string, string>> = {
	text_too_large: `dalgona refused this answer because it has more than ${MAX_ANSWER} characters. Shorten it and submit again.`,
	disallowed_control:
		"dalgona refused this answer because it has a NUL character. Remove it and submit again.",
	invalid_encoding:
		"dalgona refused this answer because it has an incomplete character. Retype it and submit again.",
	line_break_not_allowed:
		"dalgona refused this answer because it has a line break. Remove it and submit again.",
};

const correctionField = /^answers\.(.+)\.answers\[(\d+)\]$/;

function isMembers(value: unknown): value is Members {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isModelText(value: unknown): value is string {
	return typeof value === "string" && textFault(value) === null;
}

function isRequiredText(value: unknown): value is string {
	return isModelText(value) && !isBlank(value);
}

export function answerFault(text: string): AnswerFault | null {
	const fault = textFault(text);
	if (fault !== null) return fault;
	return scalarCount(text) > MAX_ANSWER ? "text_too_large" : null;
}

function parseOption(value: unknown): QuestionOption | null {
	if (!isMembers(value)) return null;
	const label = value["label"];
	const description = value["description"];
	const preview = value["preview"];
	if (!isRequiredText(label) || !isRequiredText(description)) return null;
	if (preview !== undefined && preview !== null && !isModelText(preview)) {
		return null;
	}
	return {
		label,
		description,
		preview: typeof preview === "string" ? preview : null,
	};
}

function parseOptions(value: unknown): readonly QuestionOption[] | null {
	if (value === undefined || value === null) return [];
	if (!Array.isArray(value)) return null;
	const options: QuestionOption[] = [];
	for (const entry of value) {
		const option = parseOption(entry);
		if (option === null) return null;
		options.push(option);
	}
	return options;
}

function parseQuestion(value: unknown): QuestionItem | null {
	if (!isMembers(value)) return null;
	const id = value["id"];
	const header = value["header"];
	const question = value["question"];
	const isOther = value["isOther"];
	const multiSelect = value["multiSelect"];
	if (
		typeof id !== "string" ||
		!isRequiredText(header) ||
		!isRequiredText(question) ||
		typeof isOther !== "boolean"
	) {
		return null;
	}
	if (multiSelect !== undefined && typeof multiSelect !== "boolean")
		return null;
	const options = parseOptions(value["options"]);
	if (options === null) return null;
	return {
		id,
		header,
		question,
		options,
		multiSelect: multiSelect === true,
		isOther,
	};
}

export function parseQuestionRequest(
	id: RequestId,
	params: unknown,
): QuestionRequest | null {
	if (!isMembers(params)) return null;
	const threadId = params["threadId"];
	const turnId = params["turnId"];
	const itemId = params["itemId"];
	const questions = params["questions"];
	if (
		typeof threadId !== "string" ||
		typeof turnId !== "string" ||
		typeof itemId !== "string" ||
		!Array.isArray(questions) ||
		questions.length === 0
	) {
		return null;
	}
	const parsed: QuestionItem[] = [];
	for (const entry of questions) {
		const question = parseQuestion(entry);
		if (question === null) return null;
		parsed.push(question);
	}
	return {
		id,
		method: QUESTION_METHOD,
		threadId,
		turnId,
		itemId,
		questions: parsed,
	};
}

export function isQuestionRequest(
	request: ServerRequest,
): request is QuestionRequest {
	return request.method === QUESTION_METHOD;
}

function answerTexts(
	question: QuestionItem,
	selection: QuestionSelection | undefined,
): readonly string[] {
	const picked = question.options
		.map((option) => option.label)
		.filter((label) => selection?.picked.includes(label) === true);
	const other = selection?.other ?? "";
	return other === "" ? picked : [...picked, other];
}

export function questionResponse(
	request: QuestionRequest,
	selections: readonly QuestionSelection[],
): JsonValue {
	const answers: Record<string, JsonValue> = {};
	request.questions.forEach((question, index) => {
		answers[question.id] = {
			answers: answerTexts(question, selections[index]),
		};
	});
	return { answers };
}

export function resolutionNotice(resolution: string | null): string | null {
	if (resolution === null || !Object.hasOwn(resolutionNotices, resolution)) {
		return null;
	}
	return resolutionNotices[resolution] ?? null;
}

export function parseQuestionWarning(params: unknown): QuestionWarning | null {
	if (!isMembers(params)) return null;
	const { threadId, message, requestId, reason, field } = params;
	if (
		typeof threadId !== "string" ||
		typeof message !== "string" ||
		(typeof requestId !== "string" && typeof requestId !== "number") ||
		typeof reason !== "string" ||
		typeof field !== "string"
	) {
		return null;
	}
	return { threadId, message, requestId, reason, field };
}

export function correctionOf(warning: QuestionWarning): QuestionCorrection {
	const match = correctionField.exec(warning.field);
	return {
		questionId: match?.[1] ?? null,
		answerIndex: match?.[2] === undefined ? null : Number(match[2]),
		reason: warning.reason,
	};
}

export function correctionNotice(reason: string): string {
	const notice = Object.hasOwn(correctionNotices, reason)
		? correctionNotices[reason]
		: undefined;
	return notice ?? "dalgona refused this answer. Edit it and submit again.";
}
