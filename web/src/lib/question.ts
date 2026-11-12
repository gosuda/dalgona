import type {
	JsonValue,
	QuestionItem,
	QuestionOption,
	QuestionRequest,
	QuestionSelection,
	RequestId,
	ServerRequest,
} from "./types";

export const MAX_ANSWER = 2000;

const questionMethod = "item/tool/requestUserInput";

interface Members {
	readonly [key: string]: unknown;
}

const resolutionNotices: Readonly<Record<string, string>> = {
	answered: "Question answered in another window",
	cancelled: "Question dismissed in another window",
	interrupted: "Question closed: the turn was interrupted",
	declined: "Question closed: the agent continues without an answer",
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
		method: questionMethod,
		threadId,
		turnId,
		itemId,
		questions: parsed,
	};
}

export function isQuestionRequest(
	request: ServerRequest,
): request is QuestionRequest {
	return request.method === questionMethod;
}

function answerTexts(
	question: QuestionItem,
	selection: QuestionSelection | undefined,
): readonly string[] {
	const picked = question.options
		.map((option) => option.label)
		.filter((label) => selection?.picked.includes(label) === true);
	const other = Array.from((selection?.other ?? "").trim())
		.slice(0, MAX_ANSWER)
		.join("");
	return [...picked, other].filter((text) => text !== "");
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
