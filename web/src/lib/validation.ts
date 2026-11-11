import type {
	AdvisorState,
	AdvisorStatus,
	AppNotification,
	ApprovalRequest,
	FileChange,
	InitializeInfo,
	JsonValue,
	KeywordDefinition,
	ModelOption,
	PlanStep,
	RequestId,
	StatusInfo,
	ThreadActivity,
	ThreadDetail,
	ThreadListResult,
	ThreadStartResult,
	ThreadSummary,
	TranscriptItem,
	TurnSnapshot,
	TurnStartResult,
	TurnStatus,
	TurnSteerResult,
} from "./types";

class InvalidPayload extends Error {
	constructor(label: string) {
		super(`invalid ${label}`);
		this.name = "InvalidPayload";
	}
}

function fail(label: string): never {
	throw new InvalidPayload(label);
}

function isObject(
	value: unknown,
): value is { readonly [key: string]: unknown } {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function object(
	value: unknown,
	label: string,
): { readonly [key: string]: unknown } {
	if (!isObject(value)) {
		fail(label);
	}
	return value;
}

function requiredString(
	source: { readonly [key: string]: unknown },
	key: string,
	label: string,
): string {
	const value = source[key];
	if (typeof value !== "string") {
		fail(label);
	}
	return value;
}

function requiredNumber(
	source: { readonly [key: string]: unknown },
	key: string,
	label: string,
): number {
	const value = source[key];
	if (typeof value !== "number" || !Number.isFinite(value)) {
		fail(label);
	}
	return value;
}

function textList(value: unknown): string {
	if (!Array.isArray(value)) {
		return "";
	}
	const parts: string[] = [];
	for (const entry of value) {
		if (typeof entry === "string") {
			parts.push(entry);
		}
	}
	return parts.join("");
}

export function parseJsonValue(value: unknown): JsonValue | undefined {
	if (
		value === null ||
		typeof value === "string" ||
		typeof value === "boolean"
	) {
		return value;
	}
	if (typeof value === "number") {
		return Number.isFinite(value) ? value : undefined;
	}
	if (Array.isArray(value)) {
		const items: JsonValue[] = [];
		for (const entry of value) {
			const parsed = parseJsonValue(entry);
			if (parsed === undefined) {
				return undefined;
			}
			items.push(parsed);
		}
		return items;
	}
	if (!isObject(value)) {
		return undefined;
	}
	const out: { [key: string]: JsonValue } = {};
	for (const key of Object.keys(value)) {
		const parsed = parseJsonValue(value[key]);
		if (parsed === undefined) {
			return undefined;
		}
		out[key] = parsed;
	}
	return out;
}

function activity(value: unknown): ThreadActivity {
	if (!isObject(value)) {
		return "unknown";
	}
	const kind = value["type"];
	if (
		kind === "notLoaded" ||
		kind === "idle" ||
		kind === "systemError" ||
		kind === "active"
	) {
		return kind;
	}
	return "unknown";
}

function turnStatus(value: unknown, label: string): TurnStatus {
	if (
		value === "completed" ||
		value === "interrupted" ||
		value === "failed" ||
		value === "inProgress"
	) {
		return value;
	}
	fail(label);
}

function approvalLabel(value: unknown): string {
	if (value === "never") {
		return "yolo";
	}
	if (value === "untrusted") {
		return "ask";
	}
	return "auto";
}

function sandboxLabel(value: unknown): string {
	if (value === "danger-full-access" || value === "dangerFullAccess") {
		return "off";
	}
	if (value === "workspace-write" || value === "workspaceWrite") {
		return "workspace-write";
	}
	if (isObject(value)) {
		return sandboxLabel(value["type"]);
	}
	if (typeof value === "string") {
		return value;
	}
	return "read-only";
}

function runnerLabel(value: unknown): string {
	if (typeof value === "string" && value !== "") {
		return value;
	}
	return "local";
}

function statusInfo(
	model: unknown,
	approval: unknown,
	sandbox: unknown,
	runner: unknown,
): StatusInfo {
	return {
		model: typeof model === "string" ? model : "",
		approval: approvalLabel(approval),
		sandbox: sandboxLabel(sandbox),
		runner: runnerLabel(runner),
	};
}

function userText(content: unknown): string {
	if (!Array.isArray(content)) {
		return "";
	}
	const parts: string[] = [];
	for (const entry of content) {
		if (
			isObject(entry) &&
			entry["type"] === "text" &&
			typeof entry["text"] === "string"
		) {
			parts.push(entry["text"]);
		}
	}
	return parts.join("");
}

function outputText(value: unknown): string {
	if (typeof value === "string") {
		return value;
	}
	if (!Array.isArray(value)) {
		return "";
	}
	const parts: string[] = [];
	for (const entry of value) {
		if (isObject(entry) && typeof entry["text"] === "string") {
			parts.push(entry["text"]);
		}
	}
	return parts.join("");
}

function changeKind(value: unknown): string {
	if (typeof value === "string") {
		return value;
	}
	if (isObject(value) && typeof value["type"] === "string") {
		return value["type"];
	}
	return "update";
}

function fileChanges(value: unknown): FileChange[] {
	if (!Array.isArray(value)) {
		return [];
	}
	const changes: FileChange[] = [];
	for (const entry of value) {
		if (
			!isObject(entry) ||
			typeof entry["path"] !== "string" ||
			typeof entry["diff"] !== "string"
		) {
			continue;
		}
		changes.push({
			path: entry["path"],
			diff: entry["diff"],
			kind: changeKind(entry["kind"]),
		});
	}
	return changes;
}

function toolOutput(value: unknown): string {
	if (typeof value === "string") {
		return value;
	}
	const parsed = parseJsonValue(value);
	if (parsed === undefined) {
		return "";
	}
	return JSON.stringify(parsed);
}

function transcriptItem(
	value: unknown,
	completion: "complete" | "pending",
): TranscriptItem {
	const item = object(value, "item");
	const id = requiredString(item, "id", "item");
	const kind = item["type"];
	if (kind === "userMessage") {
		return { id, kind: "userMessage", text: userText(item["content"]) };
	}
	if (kind === "agentMessage") {
		return {
			id,
			kind: "agentMessage",
			text: typeof item["text"] === "string" ? item["text"] : "",
			complete: completion === "complete",
		};
	}
	if (kind === "reasoning") {
		const content = textList(item["content"]);
		const text = content !== "" ? content : textList(item["summary"]);
		return { id, kind: "reasoning", text, complete: completion === "complete" };
	}
	if (kind === "commandExecution") {
		return {
			id,
			kind: "commandExecution",
			command: typeof item["command"] === "string" ? item["command"] : "",
			output:
				typeof item["aggregatedOutput"] === "string"
					? item["aggregatedOutput"]
					: "",
			status:
				typeof item["status"] === "string" ? item["status"] : "inProgress",
			exitCode: typeof item["exitCode"] === "number" ? item["exitCode"] : null,
		};
	}
	if (kind === "fileChange") {
		return {
			id,
			kind: "fileChange",
			changes: fileChanges(item["changes"]),
			status:
				typeof item["status"] === "string" ? item["status"] : "inProgress",
		};
	}
	if (kind === "plan") {
		return {
			id,
			kind: "plan",
			text: typeof item["text"] === "string" ? item["text"] : "",
		};
	}
	if (kind === "contextCompaction") {
		return { id, kind: "contextCompaction" };
	}
	if (kind === "dynamicToolCall") {
		return dynamicToolItem(id, item);
	}
	if (kind === "mcpToolCall") {
		const name = typeof item["tool"] === "string" ? item["tool"] : "";
		const args = parseJsonValue(item["arguments"]) ?? null;
		const result = item["result"];
		const output = isObject(result)
			? toolOutput(result["content"])
			: toolOutput(item["error"]);
		return {
			id,
			kind: "toolCall",
			name,
			arguments: args,
			output,
			status:
				typeof item["status"] === "string" ? item["status"] : "inProgress",
		};
	}
	if (kind === "functionCallOutput") {
		return {
			id,
			kind: "toolCall",
			name: typeof item["name"] === "string" ? item["name"] : "",
			arguments: null,
			output: outputText(item["output"]),
			status: "completed",
		};
	}
	return {
		id,
		kind: "unsupported",
		label: typeof kind === "string" ? kind : "unknown",
	};
}

function turnError(value: unknown): string | null {
	if (value === null || value === undefined) {
		return null;
	}
	if (!isObject(value) || typeof value["message"] !== "string") {
		return null;
	}
	return value["message"];
}

function turnSnapshot(value: unknown, label: string): TurnSnapshot {
	const turn = object(value, label);
	const status = turnStatus(turn["status"], label);
	const completion = status === "inProgress" ? "pending" : "complete";
	const items: TranscriptItem[] = [];
	if (Array.isArray(turn["items"])) {
		for (const entry of turn["items"]) {
			items.push(transcriptItem(entry, completion));
		}
	}
	return {
		id: requiredString(turn, "id", label),
		status,
		items,
		error: turnError(turn["error"]),
	};
}

function threadSummary(value: {
	readonly [key: string]: unknown;
}): ThreadSummary {
	const name = typeof value["name"] === "string" ? value["name"] : null;
	const preview = typeof value["preview"] === "string" ? value["preview"] : "";
	return {
		id: requiredString(value, "id", "thread"),
		title: name ?? preview,
		preview,
		status: activity(value["status"]),
		updatedAt: requiredNumber(value, "updatedAt", "thread"),
		model: typeof value["model"] === "string" ? value["model"] : null,
		cwd: typeof value["cwd"] === "string" ? value["cwd"] : "",
	};
}

function threadDetail(value: unknown): ThreadDetail {
	const thread = object(value, "thread");
	const turns: TurnSnapshot[] = [];
	if (Array.isArray(thread["turns"])) {
		for (const entry of thread["turns"]) {
			turns.push(turnSnapshot(entry, "turn"));
		}
	}
	return { summary: threadSummary(thread), turns };
}

export function parseThreadListResponse(value: unknown): ThreadListResult {
	const body = object(value, "thread/list");
	if (!Array.isArray(body["data"])) {
		fail("thread/list");
	}
	const threads = body["data"].map((entry) =>
		threadSummary(object(entry, "thread")),
	);
	const cursor = body["nextCursor"];
	return { threads, nextCursor: typeof cursor === "string" ? cursor : null };
}

export function parseThreadStartResponse(value: unknown): ThreadStartResult {
	const body = object(value, "thread/start");
	return {
		thread: threadDetail(body["thread"]),
		status: statusInfo(
			body["model"],
			body["approvalPolicy"],
			body["sandbox"],
			body["runner"],
		),
	};
}

export function parseThreadReadResponse(value: unknown): ThreadDetail {
	const body = object(value, "thread/read");
	return threadDetail(body["thread"]);
}

export function parseModelListResponse(value: unknown): readonly ModelOption[] {
	const body = object(value, "model/list");
	if (!Array.isArray(body["data"])) {
		fail("model/list");
	}
	return body["data"].map((entry) => {
		const model = object(entry, "model");
		return {
			id: requiredString(model, "id", "model"),
			label:
				typeof model["displayName"] === "string"
					? model["displayName"]
					: requiredString(model, "model", "model"),
			description:
				typeof model["description"] === "string" ? model["description"] : "",
			hidden: model["hidden"] === true,
		};
	});
}

export function parseConfigReadResponse(value: unknown): StatusInfo {
	const body = object(value, "config/read");
	const config = object(body["config"], "config");
	return statusInfo(
		config["model"],
		config["approval_policy"],
		config["sandbox_mode"],
		config["runner"],
	);
}

export function parseTurnStartResponse(value: unknown): TurnStartResult {
	const body = object(value, "turn/start");
	return { turn: turnSnapshot(body["turn"], "turn") };
}

export function parseTurnSteerResponse(value: unknown): TurnSteerResult {
	const body = object(value, "turn/steer");
	return { turnId: requiredString(body, "turnId", "turn/steer") };
}

export function parseKeywordsResponse(
	value: unknown,
): readonly KeywordDefinition[] {
	if (!Array.isArray(value)) {
		fail("keywords");
	}
	return value.map((entry) => {
		const row = object(entry, "keyword");
		const hue = row["hue"];
		if (!Array.isArray(hue)) fail("keyword");
		const start = hue[0];
		const end = hue[1];
		if (
			hue.length !== 2 ||
			typeof start !== "number" ||
			typeof end !== "number"
		) {
			fail("keyword");
		}
		return { word: requiredString(row, "word", "keyword"), hue: [start, end] };
	});
}

export function parseInitializeResponse(value: unknown): InitializeInfo {
	const body = object(value, "initialize");
	return {
		userAgent: requiredString(body, "userAgent", "initialize"),
		platformOs: requiredString(body, "platformOs", "initialize"),
	};
}

function dynamicToolItem(
	id: string,
	item: { readonly [key: string]: unknown },
): TranscriptItem {
	const name = typeof item["tool"] === "string" ? item["tool"] : "";
	const args = parseJsonValue(item["arguments"]) ?? null;
	const output = outputText(item["contentItems"]);
	if (name === "advisor" && isObject(args)) {
		const advisor = args["advisor"];
		const severity = args["severity"];
		return {
			id,
			kind: "advisorNote",
			advisor: typeof advisor === "string" ? advisor : name,
			severity: typeof severity === "string" ? severity : "",
			note: output,
		};
	}
	return {
		id,
		kind: "toolCall",
		name,
		arguments: args,
		output,
		status: typeof item["status"] === "string" ? item["status"] : "inProgress",
	};
}

function planStep(value: unknown): PlanStep {
	const row = object(value, "plan step");
	const status = row["status"];
	if (
		status !== "pending" &&
		status !== "inProgress" &&
		status !== "completed"
	) {
		fail("plan step");
	}
	return { step: requiredString(row, "step", "plan step"), status };
}

function advisorStatus(value: unknown): AdvisorStatus {
	if (
		value === "idle" ||
		value === "reviewing" ||
		value === "halted" ||
		value === "disabled"
	) {
		return value;
	}
	fail("advisor status");
}

export function parseAdvisorStatusResponse(
	value: unknown,
): readonly AdvisorState[] {
	if (!Array.isArray(value)) {
		fail("dalgona/advisor/status");
	}
	return value.map((entry) => {
		const row = object(entry, "advisor");
		return {
			name: requiredString(row, "name", "advisor"),
			model: typeof row["model"] === "string" ? row["model"] : "",
			status: advisorStatus(row["status"]),
			notes: requiredNumber(row, "notes", "advisor"),
		};
	});
}

export function parseVoidResponse(value: unknown): null {
	if (value === null || isObject(value)) {
		return null;
	}
	fail("void");
}

function delta(
	channel: "agent" | "reasoning" | "command",
	params: unknown,
): AppNotification | null {
	if (!isObject(params)) {
		return null;
	}
	if (
		typeof params["threadId"] !== "string" ||
		typeof params["turnId"] !== "string" ||
		typeof params["itemId"] !== "string" ||
		typeof params["delta"] !== "string"
	) {
		return null;
	}
	return {
		type: "delta",
		channel,
		threadId: params["threadId"],
		turnId: params["turnId"],
		itemId: params["itemId"],
		delta: params["delta"],
	};
}

function itemNotice(
	type: "itemStarted" | "itemCompleted",
	params: unknown,
): AppNotification | null {
	if (
		!isObject(params) ||
		typeof params["threadId"] !== "string" ||
		typeof params["turnId"] !== "string"
	) {
		return null;
	}
	return {
		type,
		threadId: params["threadId"],
		turnId: params["turnId"],
		item: transcriptItem(
			params["item"],
			type === "itemCompleted" ? "complete" : "pending",
		),
	};
}

function turnNotice(
	type: "turnStarted" | "turnCompleted",
	params: unknown,
): AppNotification | null {
	if (!isObject(params) || typeof params["threadId"] !== "string") {
		return null;
	}
	return {
		type,
		threadId: params["threadId"],
		turn: turnSnapshot(params["turn"], "turn"),
	};
}

export function parseNotification(value: unknown): AppNotification | null {
	if (!isObject(value) || typeof value["method"] !== "string") {
		return null;
	}
	const params = value["params"];
	switch (value["method"]) {
		case "thread/started": {
			if (!isObject(params)) {
				return null;
			}
			return { type: "threadStarted", thread: threadDetail(params["thread"]) };
		}
		case "thread/status/changed": {
			if (!isObject(params) || typeof params["threadId"] !== "string") {
				return null;
			}
			return {
				type: "threadStatus",
				threadId: params["threadId"],
				status: activity(params["status"]),
			};
		}
		case "thread/name/updated": {
			if (!isObject(params) || typeof params["threadId"] !== "string") {
				return null;
			}
			return {
				type: "threadName",
				threadId: params["threadId"],
				name:
					typeof params["threadName"] === "string" ? params["threadName"] : "",
			};
		}
		case "turn/started":
			return turnNotice("turnStarted", params);
		case "turn/completed":
			return turnNotice("turnCompleted", params);
		case "item/started":
			return itemNotice("itemStarted", params);
		case "item/completed":
			return itemNotice("itemCompleted", params);
		case "item/agentMessage/delta":
			return delta("agent", params);
		case "item/reasoning/textDelta":
			return delta("reasoning", params);
		case "item/commandExecution/outputDelta":
			return delta("command", params);
		case "error": {
			if (
				!isObject(params) ||
				typeof params["threadId"] !== "string" ||
				typeof params["turnId"] !== "string" ||
				typeof params["willRetry"] !== "boolean"
			) {
				return null;
			}
			return {
				type: "error",
				threadId: params["threadId"],
				turnId: params["turnId"],
				message: turnError(params["error"]) ?? "",
				willRetry: params["willRetry"],
			};
		}
		case "turn/plan/updated": {
			if (
				!isObject(params) ||
				typeof params["threadId"] !== "string" ||
				typeof params["turnId"] !== "string" ||
				!Array.isArray(params["plan"])
			) {
				return null;
			}
			const explanation = params["explanation"];
			return {
				type: "plan",
				threadId: params["threadId"],
				turnId: params["turnId"],
				explanation: typeof explanation === "string" ? explanation : null,
				steps: params["plan"].map(planStep),
			};
		}
		case "dalgona/thread/childSpawned": {
			if (
				!isObject(params) ||
				typeof params["threadId"] !== "string" ||
				typeof params["childId"] !== "string"
			) {
				return null;
			}
			return {
				type: "childSpawned",
				threadId: params["threadId"],
				childId: params["childId"],
			};
		}
		case "dalgona/advisor/note": {
			if (!isObject(params) || typeof params["threadId"] !== "string") {
				return null;
			}
			return { type: "advisorNote", threadId: params["threadId"] };
		}
		case "serverRequest/resolved": {
			if (
				!isObject(params) ||
				(typeof params["requestId"] !== "string" &&
					typeof params["requestId"] !== "number")
			) {
				return null;
			}
			return { type: "requestResolved", requestId: params["requestId"] };
		}
		default:
			return null;
	}
}

export function parseApprovalRequest(
	method: string,
	id: RequestId,
	params: unknown,
): ApprovalRequest | null {
	if (!isObject(params)) {
		return null;
	}
	if (
		typeof params["threadId"] !== "string" ||
		typeof params["turnId"] !== "string" ||
		typeof params["itemId"] !== "string"
	) {
		return null;
	}
	const reason = typeof params["reason"] === "string" ? params["reason"] : null;
	if (method === "item/commandExecution/requestApproval") {
		const command =
			typeof params["command"] === "string" ? params["command"] : "";
		return {
			id,
			method,
			threadId: params["threadId"],
			turnId: params["turnId"],
			itemId: params["itemId"],
			title: "Run command?",
			detail: command,
			reason,
		};
	}
	if (method === "item/fileChange/requestApproval") {
		const root =
			typeof params["grantRoot"] === "string" ? params["grantRoot"] : "";
		return {
			id,
			method,
			threadId: params["threadId"],
			turnId: params["turnId"],
			itemId: params["itemId"],
			title: "Apply file changes?",
			detail: root,
			reason,
		};
	}
	return null;
}
