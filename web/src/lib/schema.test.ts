import Ajv, { type ValidateFunction } from "ajv";
import { describe, expect, it } from "vitest";
import type { JsonValue } from "./types.js";
import {
	parseAdvisorStatusResponse,
	parseApprovalRequest,
	parseConfigReadResponse,
	parseKeywordsResponse,
	parseModelListResponse,
	parseNotification,
	parseThreadListResponse,
	parseThreadReadResponse,
	parseThreadStartResponse,
	parseTurnStartResponse,
	parseTurnSteerResponse,
} from "./validation.js";

interface Schema {
	readonly title?: string;
	readonly required?: readonly string[];
	readonly items?: { readonly required?: readonly string[] };
}

type JsonObject = { readonly [key: string]: JsonValue };

const codexSchemas = Object.values(
	import.meta.glob("../../../internal/rpc/testdata/codex-schema/**/*.json", {
		eager: true,
		import: "default",
	}),
) as Schema[];

const dalgonaSchemas = Object.values(
	import.meta.glob("../../../internal/rpc/testdata/dalgona-schema/*.json", {
		eager: true,
		import: "default",
	}),
) as Schema[];

const ajv = new Ajv({ strict: false, validateFormats: false });
const validators = new Map<string, ValidateFunction>();

function schemaTitled(schemas: readonly Schema[], title: string): Schema {
	const matches = schemas.filter((schema) => schema.title === title);
	const [only] = matches;
	if (matches.length !== 1 || only === undefined) {
		throw new Error(
			`expected one schema titled ${title}, found ${matches.length}`,
		);
	}
	return only;
}

function validatorFor(
	schemas: readonly Schema[],
	title: string,
): ValidateFunction {
	const cached = validators.get(title);
	if (cached !== undefined) {
		return cached;
	}
	const validate = ajv.compile(schemaTitled(schemas, title));
	validators.set(title, validate);
	return validate;
}

function without(fixture: JsonObject, key: string): JsonObject {
	return Object.fromEntries(
		Object.entries(fixture).filter(([name]) => name !== key),
	);
}

function thrownBy(run: () => object | null): Error | null {
	try {
		run();
		return null;
	} catch (error) {
		return error instanceof Error ? error : new Error(String(error));
	}
}

const thread = {
	cliVersion: "0.1.0",
	createdAt: 1,
	cwd: "/work",
	ephemeral: false,
	id: "thread-1",
	modelProvider: "openai",
	preview: "hello",
	projectId: "project-1",
	sessionId: "session-1",
	source: "cli",
	status: { type: "idle" },
	turns: [],
	updatedAt: 2,
};

const turn = { id: "turn-1", items: [], status: "completed" };

const agentItem = { id: "item-1", text: "hi", type: "agentMessage" };

const sandbox = {
	type: "workspaceWrite",
	writableRoots: [],
	networkAccess: false,
	excludeTmpdirEnvVar: false,
	excludeSlashTmp: false,
};

const model = {
	defaultReasoningEffort: "medium",
	description: "A model",
	displayName: "GPT",
	hidden: false,
	id: "gpt-test",
	isDefault: true,
	model: "gpt-test",
	supportedReasoningEfforts: [],
};

const threadResponse = {
	approvalPolicy: "never",
	approvalsReviewer: "user",
	cwd: "/work",
	model: "gpt-test",
	modelProvider: "openai",
	sandbox,
	thread,
};

type Kind = "notification" | "request" | "response";

interface CodexCase {
	readonly kind: Kind;
	readonly title: string;
	readonly fixture: JsonObject;
	readonly parse: (value: JsonValue) => object | null;
}

function notification(
	method: string,
	title: string,
	fixture: JsonObject,
): CodexCase {
	return {
		kind: "notification",
		title,
		fixture,
		parse: (params) => parseNotification({ method, params }),
	};
}

function request(
	method: string,
	title: string,
	fixture: JsonObject,
): CodexCase {
	return {
		kind: "request",
		title,
		fixture,
		parse: (params) => parseApprovalRequest(method, "req-1", params),
	};
}

function response(
	title: string,
	fixture: JsonObject,
	parse: (value: JsonValue) => object | null,
): CodexCase {
	return { kind: "response", title, fixture, parse };
}

const codexCases: readonly CodexCase[] = [
	notification("thread/started", "ThreadStartedNotification", { thread }),
	notification("thread/status/changed", "ThreadStatusChangedNotification", {
		status: { type: "idle" },
		threadId: "thread-1",
	}),
	notification("thread/name/updated", "ThreadNameUpdatedNotification", {
		threadId: "thread-1",
		threadName: "Plan",
	}),
	notification("turn/started", "TurnStartedNotification", {
		threadId: "thread-1",
		turn,
	}),
	notification("turn/completed", "TurnCompletedNotification", {
		threadId: "thread-1",
		turn,
	}),
	notification("item/started", "ItemStartedNotification", {
		item: agentItem,
		startedAtMs: 1,
		threadId: "thread-1",
		turnId: "turn-1",
	}),
	notification("item/completed", "ItemCompletedNotification", {
		completedAtMs: 2,
		item: agentItem,
		threadId: "thread-1",
		turnId: "turn-1",
	}),
	notification("item/agentMessage/delta", "AgentMessageDeltaNotification", {
		delta: "hi",
		itemId: "item-1",
		threadId: "thread-1",
		turnId: "turn-1",
	}),
	notification("item/reasoning/textDelta", "ReasoningTextDeltaNotification", {
		contentIndex: 0,
		delta: "think",
		itemId: "item-2",
		threadId: "thread-1",
		turnId: "turn-1",
	}),
	notification(
		"item/commandExecution/outputDelta",
		"CommandExecutionOutputDeltaNotification",
		{
			delta: "ok\n",
			itemId: "item-3",
			threadId: "thread-1",
			turnId: "turn-1",
		},
	),
	notification("error", "ErrorNotification", {
		error: { message: "boom" },
		threadId: "thread-1",
		turnId: "turn-1",
		willRetry: false,
	}),
	notification("turn/plan/updated", "TurnPlanUpdatedNotification", {
		explanation: null,
		plan: [{ status: "completed", step: "read" }],
		threadId: "thread-1",
		turnId: "turn-1",
	}),
	notification(
		"thread/tokenUsage/updated",
		"ThreadTokenUsageUpdatedNotification",
		{
			threadId: "thread-1",
			turnId: "turn-1",
			tokenUsage: {
				last: {
					totalTokens: 8,
					inputTokens: 5,
					cachedInputTokens: 0,
					cacheWriteInputTokens: 0,
					outputTokens: 3,
					reasoningOutputTokens: 0,
				},
				total: {
					totalTokens: 8,
					inputTokens: 5,
					cachedInputTokens: 0,
					cacheWriteInputTokens: 0,
					outputTokens: 3,
					reasoningOutputTokens: 0,
				},
				modelContextWindow: 200000,
			},
		},
	),
	notification("serverRequest/resolved", "ServerRequestResolvedNotification", {
		requestId: "req-1",
		threadId: "thread-1",
	}),
	request(
		"item/commandExecution/requestApproval",
		"CommandExecutionRequestApprovalParams",
		{
			command: "ls",
			itemId: "item-4",
			startedAtMs: 3,
			threadId: "thread-1",
			turnId: "turn-1",
		},
	),
	request(
		"item/fileChange/requestApproval",
		"FileChangeRequestApprovalParams",
		{
			grantRoot: "/work",
			itemId: "item-5",
			startedAtMs: 4,
			threadId: "thread-1",
			turnId: "turn-1",
		},
	),
	response(
		"ThreadListResponse",
		{ data: [], nextCursor: null },
		parseThreadListResponse,
	),
	response("ThreadStartResponse", threadResponse, parseThreadStartResponse),
	response("ThreadResumeResponse", threadResponse, parseThreadStartResponse),
	response("ThreadReadResponse", { thread }, parseThreadReadResponse),
	response("TurnStartResponse", { turn }, parseTurnStartResponse),
	response("TurnSteerResponse", { turnId: "turn-1" }, parseTurnSteerResponse),
	response("ModelListResponse", { data: [model] }, parseModelListResponse),
	response(
		"ConfigReadResponse",
		{ config: {}, origins: {} },
		parseConfigReadResponse,
	),
];

const helloThread = {
	summary: {
		id: "thread-1",
		title: "hello",
		preview: "hello",
		status: "idle",
		activeFlags: [],
		updatedAt: 2,
		model: null,
		cwd: "/work",
	},
	turns: [],
};

const completedTurn = {
	id: "turn-1",
	status: "completed",
	items: [],
	error: null,
	startedAtMs: null,
};

const agentMessage = {
	id: "item-1",
	kind: "agentMessage",
	text: "hi",
};

const threadStartValue = {
	thread: helloThread,
	status: {
		model: "gpt-test",
		approval: "yolo",
		sandbox: "workspace-write",
		effort: null,
	},
	usage: null,
};

const expectedParsed: Readonly<Record<string, JsonValue>> = {
	ThreadStartedNotification: { type: "threadStarted", thread: helloThread },
	ThreadStatusChangedNotification: {
		type: "threadStatus",
		threadId: "thread-1",
		seq: 0,
		status: "idle",
		activeFlags: [],
	},
	ThreadNameUpdatedNotification: {
		type: "threadName",
		threadId: "thread-1",
		name: "Plan",
	},
	TurnStartedNotification: {
		type: "turnStarted",
		threadId: "thread-1",
		turn: completedTurn,
	},
	TurnCompletedNotification: {
		type: "turnCompleted",
		threadId: "thread-1",
		turn: completedTurn,
	},
	ItemStartedNotification: {
		type: "itemStarted",
		threadId: "thread-1",
		turnId: "turn-1",
		item: { ...agentMessage, complete: false },
	},
	ItemCompletedNotification: {
		type: "itemCompleted",
		threadId: "thread-1",
		turnId: "turn-1",
		item: { ...agentMessage, complete: true },
	},
	AgentMessageDeltaNotification: {
		type: "delta",
		channel: "agent",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "item-1",
		delta: "hi",
	},
	ReasoningTextDeltaNotification: {
		type: "delta",
		channel: "reasoning",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "item-2",
		delta: "think",
	},
	CommandExecutionOutputDeltaNotification: {
		type: "delta",
		channel: "command",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "item-3",
		delta: "ok\n",
	},
	ErrorNotification: {
		type: "error",
		threadId: "thread-1",
		turnId: "turn-1",
		message: "boom",
		willRetry: false,
	},
	TurnPlanUpdatedNotification: {
		type: "plan",
		threadId: "thread-1",
		turnId: "turn-1",
		explanation: null,
		steps: [{ step: "read", status: "completed" }],
	},
	ServerRequestResolvedNotification: {
		type: "requestResolved",
		requestId: "req-1",
	},
	CommandExecutionRequestApprovalParams: {
		id: "req-1",
		method: "item/commandExecution/requestApproval",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "item-4",
		title: "Run command?",
		detail: "ls",
		reason: null,
	},
	FileChangeRequestApprovalParams: {
		id: "req-1",
		method: "item/fileChange/requestApproval",
		threadId: "thread-1",
		turnId: "turn-1",
		itemId: "item-5",
		title: "Apply file changes?",
		detail: "/work",
		reason: null,
	},
	ThreadListResponse: { threads: [], nextCursor: null },
	ThreadStartResponse: threadStartValue,
	ThreadResumeResponse: threadStartValue,
	ThreadReadResponse: helloThread,
	TurnStartResponse: { turn: completedTurn },
	TurnSteerResponse: { turnId: "turn-1" },
	ThreadTokenUsageUpdatedNotification: {
		type: "tokenUsage",
		threadId: "thread-1",
		turnId: "turn-1",
		usage: {
			last: {
				totalTokens: 8,
				inputTokens: 5,
				cachedInputTokens: 0,
				cacheWriteInputTokens: 0,
				outputTokens: 3,
				reasoningOutputTokens: 0,
			},
			total: {
				totalTokens: 8,
				inputTokens: 5,
				cachedInputTokens: 0,
				cacheWriteInputTokens: 0,
				outputTokens: 3,
				reasoningOutputTokens: 0,
			},
			contextWindow: 200000,
		},
	},
	ModelListResponse: [
		{
			id: "gpt-test",
			label: "GPT",
			description: "A model",
			hidden: false,
			efforts: [],
			defaultEffort: "medium",
		},
	],
	ConfigReadResponse: {
		model: "",
		approval: "auto",
		sandbox: "read-only",
		effort: null,
	},
};

describe("codex schema conformance", () => {
	it("every fixture is a valid Codex payload", () => {
		for (const testCase of codexCases) {
			const validate = validatorFor(codexSchemas, testCase.title);
			expect(validate(testCase.fixture), testCase.title).toBe(true);
		}
	}, 30_000);

	it("each parser returns the stated value for its valid fixture", () => {
		for (const testCase of codexCases) {
			expect(testCase.title in expectedParsed, testCase.title).toBe(true);
			expect(testCase.parse(testCase.fixture), testCase.title).toEqual(
				expectedParsed[testCase.title],
			);
		}
	});

	it("each parser rejects a payload missing a required member", () => {
		for (const testCase of codexCases) {
			const validate = validatorFor(codexSchemas, testCase.title);
			const required =
				schemaTitled(codexSchemas, testCase.title).required ?? [];
			for (const key of required) {
				const payload = without(testCase.fixture, key);
				const label = `${testCase.title} without ${key}`;
				expect(validate(payload), label).toBe(false);
				if (testCase.kind === "notification") {
					expect(testCase.parse(payload), label).toBeNull();
				} else {
					expect(
						thrownBy(() => testCase.parse(payload)),
						label,
					).toMatchObject({
						name: "InvalidPayload",
					});
				}
			}
		}
	});
});

const advisorNote = {
	advisor: "advisor",
	delivery: "steer",
	note: "stop and run the tests",
	severity: "blocker",
	threadId: "thread-1",
};

const childSpawned = { childId: "child-1", cwd: "/work", threadId: "thread-1" };

const advisorStatus: JsonValue = [
	{
		model: "gpt-test",
		name: "advisor",
		notes: 0,
		status: "idle",
		usage: { inputTokens: 0, outputTokens: 0 },
	},
];

const keywords: JsonValue = [{ hue: [0, 330], word: "ultrathink" }];

describe("dalgona schema conformance", () => {
	const dalgonaFixtures: readonly {
		readonly title: string;
		readonly fixture: JsonValue;
	}[] = [
		{ fixture: advisorNote, title: "dalgona/advisor/note" },
		{
			fixture: { threadId: "thread-1" },
			title: "dalgona/advisor/reset params",
		},
		{ fixture: {}, title: "dalgona/advisor/reset result" },
		{
			fixture: { enabled: true, threadId: "thread-1" },
			title: "dalgona/advisor/set params",
		},
		{ fixture: {}, title: "dalgona/advisor/set result" },
		{
			fixture: { threadId: "thread-1" },
			title: "dalgona/advisor/status params",
		},
		{ fixture: advisorStatus, title: "dalgona/advisor/status result" },
		{ fixture: {}, title: "dalgona/keywords/list params" },
		{ fixture: keywords, title: "dalgona/keywords/list result" },
		{
			fixture: { remainingMs: 1000, threadId: "thread-1", turnId: "turn-1" },
			title: "dalgona/retry/wait",
		},
		{ fixture: childSpawned, title: "dalgona/thread/childSpawned" },
		{
			fixture: { threadId: "thread-1", turnId: "turn-1" },
			title: "dalgona/thread/leaf/set params",
		},
		{ fixture: {}, title: "dalgona/thread/leaf/set result" },
	];

	it("validates each dalgona fixture against its schema", () => {
		for (const { title, fixture } of dalgonaFixtures) {
			const validate = validatorFor(dalgonaSchemas, title);
			expect(validate(fixture), title).toBe(true);
		}
	});

	it("rejects a dalgona notification missing a required member", () => {
		const advisorRequired =
			schemaTitled(dalgonaSchemas, "dalgona/advisor/note").required ?? [];
		for (const key of advisorRequired) {
			expect(
				parseNotification({
					method: "dalgona/advisor/note",
					params: without(advisorNote, key),
				}),
				key,
			).toBeNull();
		}
		const childRequired =
			schemaTitled(dalgonaSchemas, "dalgona/thread/childSpawned").required ??
			[];
		for (const key of childRequired) {
			expect(
				parseNotification({
					method: "dalgona/thread/childSpawned",
					params: without(childSpawned, key),
				}),
				key,
			).toBeNull();
		}
	});

	it("rejects an advisor status row missing a usage member", () => {
		expect(() =>
			parseAdvisorStatusResponse([
				without(advisorStatus[0] as JsonObject, "usage"),
			]),
		).toThrow("invalid advisor status");
	});

	it("reads the word of a keyword row and ignores its hue", () => {
		expect(parseKeywordsResponse([{ hue: [0, 330], word: "x" }])).toEqual([
			{ word: "x" },
		]);
	});

	it("rejects a keyword row without a word", () => {
		expect(() => parseKeywordsResponse([{ hue: [0, 330] }])).toThrow(
			"invalid keyword",
		);
	});

	it("rejects an advisor status row missing any required member", () => {
		const row = advisorStatus[0] as JsonObject;
		const required =
			schemaTitled(dalgonaSchemas, "dalgona/advisor/status result").items
				?.required ?? [];
		expect(required.length).toBeGreaterThan(0);
		for (const key of required) {
			expect(
				() => parseAdvisorStatusResponse([without(row, key)]),
				key,
			).toThrow("invalid advisor status");
		}
	});
});
