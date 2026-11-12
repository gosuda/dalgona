import type {
	AdvisorState,
	AdvisorStatus,
	AppNotification,
	ApprovalRequest,
	FileChange,
	InitializeInfo,
	JsonValue,
	KeywordDefinition,
	KeywordMatch,
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

function hasMembers(
	source: { readonly [key: string]: unknown },
	keys: readonly string[],
): boolean {
	return keys.every((key) => source[key] !== undefined);
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

function threadFlags(value: unknown): string[] {
	if (!isObject(value) || !Array.isArray(value["activeFlags"])) return [];
	return value["activeFlags"].filter(
		(flag): flag is string => typeof flag === "string",
	);
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

function fileCount(value: unknown): number | undefined | null {
	if (value === undefined) {
		return undefined;
	}
	return typeof value === "number" && Number.isInteger(value) && value >= 0
		? value
		: null;
}

function fileChanges(value: unknown): FileChange[] {
	if (!Array.isArray(value)) {
		return [];
	}
	const changes: FileChange[] = [];
	for (const entry of value) {
		if (!isObject(entry)) {
			continue;
		}
		const additions = fileCount(entry["additions"]);
		const deletions = fileCount(entry["deletions"]);
		const omitted = entry["diffOmitted"];
		if (
			typeof entry["path"] !== "string" ||
			typeof entry["diff"] !== "string" ||
			additions === null ||
			deletions === null ||
			(omitted !== undefined && typeof omitted !== "boolean")
		) {
			continue;
		}
		changes.push({
			path: entry["path"],
			diff: entry["diff"],
			kind: changeKind(entry["kind"]),
			diffOmitted: omitted === true,
			...(additions === undefined ? {} : { additions }),
			...(deletions === undefined ? {} : { deletions }),
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

function durationValue(item: {
	readonly [key: string]: unknown;
}): number | null {
	return typeof item["durationMs"] === "number" ? item["durationMs"] : null;
}

function transcriptItem(
	value: unknown,
	completion: "complete" | "pending",
): TranscriptItem {
	const item = object(value, "item");
	const id = requiredString(item, "id", "item");
	const kind = item["type"];
	if (kind === "userMessage") {
		return {
			id,
			kind: "userMessage",
			text: userText(item["content"]),
			turnId: "",
			keywords: keywordMatches(item["keywords"]),
		};
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
			durationMs: durationValue(item),
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
			durationMs: durationValue(item),
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
			durationMs: null,
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
	if (!hasMembers(turn, ["id", "items", "status"])) fail(label);
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
		startedAtMs:
			typeof turn["startedAt"] === "number" ? turn["startedAt"] * 1000 : null,
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
		activeFlags: threadFlags(value["status"]),
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
	if (
		!hasMembers(body, [
			"approvalPolicy",
			"approvalsReviewer",
			"cwd",
			"model",
			"modelProvider",
			"sandbox",
			"thread",
		])
	) {
		fail("thread/start");
	}
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
	if (!hasMembers(body, ["config", "origins"])) fail("config/read");
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
	return value.map((entry) => ({
		word: requiredString(object(entry, "keyword"), "word", "keyword"),
	}));
}

export function parseKeywordMatches(value: unknown): readonly KeywordMatch[] {
	return keywordMatches(object(value, "keyword matches")["matches"]);
}

function keywordMatches(value: unknown): readonly KeywordMatch[] {
	if (!Array.isArray(value)) return [];
	const matches: KeywordMatch[] = [];
	for (const entry of value) {
		if (!isObject(entry)) continue;
		const word = entry["word"];
		const range = entry["byteRange"];
		if (typeof word !== "string" || !isObject(range)) continue;
		const start = range["start"];
		const end = range["end"];
		if (typeof start !== "number" || typeof end !== "number") continue;
		matches.push({ word, byteRange: { start, end } });
	}
	return matches;
}

export interface SettingValue {
	readonly key: string;
	readonly value: JsonValue;
}

export function parseSettingsReadResponse(
	value: unknown,
): readonly SettingValue[] {
	const body = object(value, "dalgona/settings/read");
	if (!hasMembers(body, ["settings"])) fail("dalgona/settings/read");
	const settings = body["settings"];
	if (!Array.isArray(settings)) fail("dalgona/settings/read");
	return settings.map((entry) => {
		const row = object(entry, "setting");
		if (!hasMembers(row, ["key", "value"])) fail("setting");
		const parsed = parseJsonValue(row["value"]);
		if (parsed === undefined) fail("setting");
		return { key: requiredString(row, "key", "setting"), value: parsed };
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
		durationMs: durationValue(item),
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
		const row = object(entry, "advisor status");
		if (!hasMembers(row, ["name", "model", "status", "notes", "usage"])) {
			fail("advisor status");
		}
		if (
			!hasMembers(object(row["usage"], "advisor status"), [
				"inputTokens",
				"outputTokens",
			])
		) {
			fail("advisor status");
		}
		return {
			name: requiredString(row, "name", "advisor status"),
			model: requiredString(row, "model", "advisor status"),
			status: advisorStatus(row["status"]),
			notes: requiredNumber(row, "notes", "advisor status"),
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

type Members = { readonly [key: string]: unknown };

interface NotificationSpec {
	readonly required: readonly string[];
	readonly parse: (params: Members) => AppNotification | null;
}

function threadStatusNotice(params: Members): AppNotification | null {
	if (typeof params["threadId"] !== "string") {
		return null;
	}
	return {
		type: "threadStatus",
		threadId: params["threadId"],
		seq: typeof params["seq"] === "number" ? params["seq"] : 0,
		status: activity(params["status"]),
		activeFlags: threadFlags(params["status"]),
	};
}

function retryWaitNotice(params: Members): AppNotification | null {
	const remainingMs = params["remainingMs"];
	if (
		typeof params["threadId"] !== "string" ||
		typeof params["turnId"] !== "string" ||
		typeof remainingMs !== "number" ||
		remainingMs < 0
	) {
		return null;
	}
	return {
		type: "retryWait",
		threadId: params["threadId"],
		turnId: params["turnId"],
		remainingMs,
	};
}

function threadNameNotice(params: Members): AppNotification | null {
	if (typeof params["threadId"] !== "string") {
		return null;
	}
	return {
		type: "threadName",
		threadId: params["threadId"],
		name: typeof params["threadName"] === "string" ? params["threadName"] : "",
	};
}

function threadStartedNotice(params: Members): AppNotification {
	return { type: "threadStarted", thread: threadDetail(params["thread"]) };
}

function errorNotice(params: Members): AppNotification | null {
	if (
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

function planNotice(params: Members): AppNotification | null {
	if (
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

function childSpawnedNotice(params: Members): AppNotification | null {
	if (
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

function advisorNoteNotice(params: Members): AppNotification | null {
	if (typeof params["threadId"] !== "string") {
		return null;
	}
	return { type: "advisorNote", threadId: params["threadId"] };
}

function requestResolvedNotice(params: Members): AppNotification | null {
	const requestId = params["requestId"];
	if (typeof requestId !== "string" && typeof requestId !== "number") {
		return null;
	}
	return { type: "requestResolved", requestId };
}

const notifications: Readonly<Record<string, NotificationSpec>> = {
	"thread/started": { required: ["thread"], parse: threadStartedNotice },
	"thread/status/changed": {
		required: ["status", "threadId"],
		parse: threadStatusNotice,
	},
	"thread/name/updated": {
		required: ["threadId"],
		parse: threadNameNotice,
	},
	"turn/started": {
		required: ["threadId", "turn"],
		parse: (params) => turnNotice("turnStarted", params),
	},
	"turn/completed": {
		required: ["threadId", "turn"],
		parse: (params) => turnNotice("turnCompleted", params),
	},
	"item/started": {
		required: ["item", "startedAtMs", "threadId", "turnId"],
		parse: (params) => itemNotice("itemStarted", params),
	},
	"item/completed": {
		required: ["completedAtMs", "item", "threadId", "turnId"],
		parse: (params) => itemNotice("itemCompleted", params),
	},
	"item/agentMessage/delta": {
		required: ["delta", "itemId", "threadId", "turnId"],
		parse: (params) => delta("agent", params),
	},
	"item/reasoning/textDelta": {
		required: ["contentIndex", "delta", "itemId", "threadId", "turnId"],
		parse: (params) => delta("reasoning", params),
	},
	"item/commandExecution/outputDelta": {
		required: ["delta", "itemId", "threadId", "turnId"],
		parse: (params) => delta("command", params),
	},
	error: {
		required: ["error", "threadId", "turnId", "willRetry"],
		parse: errorNotice,
	},
	"turn/plan/updated": {
		required: ["plan", "threadId", "turnId"],
		parse: planNotice,
	},
	"dalgona/thread/childSpawned": {
		required: ["childId", "cwd", "threadId"],
		parse: childSpawnedNotice,
	},
	"dalgona/advisor/note": {
		required: ["advisor", "delivery", "note", "severity", "threadId"],
		parse: advisorNoteNotice,
	},
	"dalgona/retry/wait": {
		required: ["remainingMs", "threadId", "turnId"],
		parse: retryWaitNotice,
	},
	"serverRequest/resolved": {
		required: ["requestId", "threadId"],
		parse: requestResolvedNotice,
	},
};

export function parseNotification(value: unknown): AppNotification | null {
	if (!isObject(value) || typeof value["method"] !== "string") {
		return null;
	}
	const method = value["method"];
	const params = value["params"];
	const spec = Object.hasOwn(notifications, method)
		? notifications[method]
		: undefined;
	if (spec === undefined || !isObject(params)) {
		return null;
	}
	return hasMembers(params, spec.required) ? spec.parse(params) : null;
}

export function parseApprovalRequest(
	method: string,
	id: RequestId,
	params: unknown,
): ApprovalRequest | null {
	if (method !== commandApprovalMethod && method !== fileApprovalMethod) {
		return null;
	}
	const body = object(params, "approval request");
	if (!hasMembers(body, approvalMembers)) fail("approval request");
	const threadId = requiredString(body, "threadId", "approval request");
	const turnId = requiredString(body, "turnId", "approval request");
	const itemId = requiredString(body, "itemId", "approval request");
	const reason = typeof body["reason"] === "string" ? body["reason"] : null;
	if (method === commandApprovalMethod) {
		const command = typeof body["command"] === "string" ? body["command"] : "";
		return {
			id,
			method,
			threadId,
			turnId,
			itemId,
			title: "Run command?",
			detail: command,
			reason,
		};
	}
	const root = typeof body["grantRoot"] === "string" ? body["grantRoot"] : "";
	return {
		id,
		method,
		threadId,
		turnId,
		itemId,
		title: "Apply file changes?",
		detail: root,
		reason,
	};
}

const commandApprovalMethod = "item/commandExecution/requestApproval";
const fileApprovalMethod = "item/fileChange/requestApproval";
const approvalMembers = ["itemId", "startedAtMs", "threadId", "turnId"];
