import { answerScalars, checkText, type TextReason } from "./text/validate";
import type {
	JsonValue,
	QuestionItem,
	QuestionOption,
	QuestionRequest,
	QuestionSelection,
	RequestId,
	ServerRequest,
} from "./types";
import { QUESTION_METHOD } from "./types";

interface Members {
	readonly [key: string]: unknown;
}

const textErrors: Readonly<Record<TextReason, string>> = {
	text_too_large: `Shorten this answer to ${answerScalars} characters or fewer.`,
	disallowed_control: "Remove the NUL character from this answer.",
	invalid_encoding: "Retype this answer: it holds an incomplete character.",
	line_break_not_allowed: "Remove the line break from this answer.",
	invalid_offset: "Retype this answer.",
};

const resolutionNotices: Readonly<Record<string, string>> = {
	answered: "Question answered in another window",
	cancelled: "Question dismissed in another window",
	interrupted: "Question closed: the turn was interrupted",
	declined: "Question closed: the agent continues without an answer",
	timedOut: "Question timed out",
};

function isMembers(value: unknown): value is Members {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseOption(value: unknown): QuestionOption | null {
	if (!isMembers(value)) return null;
	const label = value["label"];
	const description = value["description"];
	const preview = value["preview"];
	if (typeof label !== "string" || typeof description !== "string") return null;
	if (
		preview !== undefined &&
		preview !== null &&
		typeof preview !== "string"
	) {
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
		typeof header !== "string" ||
		typeof question !== "string" ||
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
	return [...picked, selection?.other ?? ""].filter((text) => text !== "");
}

export function answerError(text: string): string | null {
	const reason = checkText(text, { maxScalars: answerScalars });
	return reason === undefined ? null : textErrors[reason];
}

export function correctionText(reason: string, message: string): string {
	return Object.hasOwn(textErrors, reason)
		? textErrors[reason as TextReason]
		: message;
}

export function correctedQuestion(field: string): string | null {
	return /^answers\.(.+)\.answers\[\d+\]$/s.exec(field)?.[1] ?? null;
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

export function resolutionNotice(
	resolution: string | null,
	submitted: boolean,
): string | null {
	if (resolution === null || !Object.hasOwn(resolutionNotices, resolution)) {
		return null;
	}
	if (submitted && resolution === "answered") return "Question answered";
	return resolutionNotices[resolution] ?? null;
}
