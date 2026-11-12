import type { PanelKind } from "./commands";
import { threadTitle } from "./labels";
import { RpcError, rpcSocketUrl, tokenFromHash } from "./protocol";
import { questionResponse, resolutionNotice } from "./question";
import {
	acceptThreadStatus,
	activeRequest,
	applyOpenThreadNotification,
	dropSentFollowup,
	emptyTranscript,
	forgetPatchChanges,
	itemsAwaitingApproval,
	type KnownChildThreads,
	keepPendingPatchChanges,
	lastTurnStatus,
	type PatchChanges,
	patchChangesFor,
	recordPatchChanges,
	recordThreadTurn,
	requestsElsewhere,
	resolveRequest,
	trackRequest,
	transcriptForThread,
} from "./reducer";
import { turnSteerParams } from "./requests";
import type { SettingEdit } from "./settings";
import {
	applyTheme,
	parseThemeSetting,
	type ThemeSetting,
} from "./theme/theme";
import type {
	AdvisorSummary,
	AppNotification,
	ApprovalRequest,
	ConnectionState,
	FileChange,
	JsonValue,
	KeywordDefinition,
	KeywordMatch,
	LoginFlow,
	LoginMethod,
	LoginProvider,
	ModelOption,
	Notice,
	NoticeAction,
	NoticeTone,
	OutputPage,
	QuestionRequest,
	QuestionSelection,
	RequestId,
	RetryWait,
	RpcHandlers,
	ServerRequest,
	StatusInfo,
	ThreadDetail,
	ThreadSummary,
	TokenUsage,
	TranscriptState,
	WaitingElsewhere,
} from "./types";
import type { SettingValue } from "./validation";
import {
	parseAdvisorStatusResponse,
	parseConfigReadResponse,
	parseInitializeResponse,
	parseKeywordMatches,
	parseKeywordsResponse,
	parseLoginProvidersResponse,
	parseLoginStartResponse,
	parseModelListResponse,
	parseOutputReadResponse,
	parseSettingsReadResponse,
	parseThreadListResponse,
	parseThreadReadResponse,
	parseThreadStartResponse,
	parseTurnStartResponse,
	parseTurnSteerResponse,
	parseVoidResponse,
} from "./validation";
import { parseVerbosity, type Verbosity } from "./verbosity";

export interface RpcPort {
	call<T>(
		method: string,
		params: JsonValue,
		parse: (value: unknown) => T,
	): Promise<T>;
	notify(method: string, params: JsonValue): void;
	respond(id: RequestId, result: JsonValue): void;
	open(): Promise<void>;
	close(): void;
}

export interface SessionLocation {
	readonly hash: string;
	readonly host: string;
	readonly pathname: string;
	readonly protocol: string;
	readonly search: string;
}

function applyThemeToPage(setting: ThemeSetting): void {
	if (typeof document === "undefined") return;
	applyTheme(setting, document, localStorage);
}

const MAX_NOTICES = 5;

type Decision = "accept" | "acceptForSession" | "decline" | "cancel";

type Effect<K extends AppNotification["type"]> = (
	notification: Extract<AppNotification, { readonly type: K }>,
) => void;

const effectKinds = [
	"threadStarted",
	"threadStatus",
	"threadName",
	"advisorNote",
	"requestResolved",
	"retryWait",
	"tokenUsage",
	"fileChangesUpdated",
	"loginCompleted",
] as const;

type EffectKind = (typeof effectKinds)[number];

const reducerKinds = [
	"turnStarted",
	"turnCompleted",
	"itemStarted",
	"itemCompleted",
	"delta",
	"error",
	"plan",
	"childSpawned",
	"toolJudged",
	"turnJudged",
] as const;

type ReducerKind = (typeof reducerKinds)[number];

export const everyNotificationListed: [
	Exclude<AppNotification["type"], EffectKind | ReducerKind>,
] extends [never]
	? true
	: never = true;

type Effects = { readonly [K in EffectKind]: Effect<K> };

function isEffectKind(type: AppNotification["type"]): type is EffectKind {
	return (effectKinds as readonly string[]).includes(type);
}

export class Session {
	private readonly dial: (url: string, handlers: RpcHandlers) => RpcPort;
	private readonly applyThemeSetting: (setting: ThemeSetting) => void;
	private location: SessionLocation = {
		hash: "",
		host: "",
		pathname: "/",
		protocol: "http:",
		search: "",
	};
	private client: RpcPort | undefined;
	private connectionGeneration = 0;
	private selectionGeneration = 0;
	private accessToken = "";
	private readonly effects: Effects;

	connection: ConnectionState = $state("idle");
	threads: readonly ThreadSummary[] = $state([]);
	activeThreadId: string | null = $state(null);
	transcript: TranscriptState = $state(emptyTranscript(null));
	transcriptRevision: number = $state(0);
	models: readonly ModelOption[] = $state([]);
	selectedModel: string = $state("");
	status: StatusInfo = $state({
		model: "Default",
		approval: "auto",
		sandbox: "workspace-write",
		effort: null,
	});
	usage: Readonly<Record<string, TokenUsage>> = $state({});
	statusHide: readonly string[] = $state([]);
	userAgent: string | null = $state(null);
	keywords: readonly KeywordDefinition[] = $state([]);
	advisor: AdvisorSummary = $state({ kind: "unknown" });
	requests: readonly ServerRequest[] = $state([]);
	patchChanges: PatchChanges = $state({});
	knownChildren: KnownChildThreads = $state({});
	queuedFollowups: readonly string[] = $state([]);
	sentTexts: readonly string[] = $state([]);
	loadingThreads: boolean = $state(false);
	loadingTranscript: boolean = $state(false);
	startingThread: boolean = $state(false);
	notices: readonly Notice[] = $state([]);
	statusSeq: Readonly<Record<string, number>> = $state({});
	retryWait: RetryWait | null = $state(null);
	completedThreadIds: ReadonlySet<string> = $state(new Set());
	verbosity: Verbosity = $state("medium");
	settingValues: readonly SettingValue[] = $state([]);
	panel: PanelKind | null = $state(null);
	loginProviders: readonly LoginProvider[] = $state([]);
	login: LoginFlow | null = $state.raw(null);
	private noticeSeq = 0;

	running: boolean = $derived(this.transcript.turnStatus === "inProgress");
	retryRemainingMs: number | null = $derived.by(() => {
		const wait = this.retryWait;
		if (
			wait === null ||
			wait.threadId !== this.activeThreadId ||
			wait.turnId !== this.transcript.turnId
		) {
			return null;
		}
		return wait.remainingMs;
	});
	currentRequest: ServerRequest | null = $derived(
		activeRequest(
			this.requests,
			this.activeThreadId,
			this.transcript.childThreadIds,
		),
	);
	waitingElsewhere: WaitingElsewhere = $derived.by(() => {
		const waiting = requestsElsewhere(
			this.requests,
			this.activeThreadId,
			this.transcript.childThreadIds,
		);
		return { count: waiting.length, threadId: waiting[0]?.threadId ?? null };
	});
	awaitingApproval: ReadonlySet<string> = $derived(
		itemsAwaitingApproval(
			this.requests,
			this.activeThreadId,
			this.transcript.childThreadIds,
		),
	);
	approvalChanges: readonly FileChange[] = $derived.by(() => {
		const request = this.currentRequest;
		if (request === null) return [];
		const changes = patchChangesFor(
			this.patchChanges,
			request.threadId,
			request.itemId,
		);
		if (changes !== null) return changes;
		const item = this.transcript.items.find(
			(entry) => entry.id === request.itemId,
		);
		return item?.kind === "fileChange" ? item.changes : [];
	});
	activeTitle: string = $derived.by(() => {
		const thread = this.threads.find(
			(entry) => entry.id === this.activeThreadId,
		);
		return thread === undefined ? "No thread selected" : threadTitle(thread);
	});

	history: readonly string[] = $derived.by(() => {
		const texts: string[] = [];
		const prompts = this.transcript.items.flatMap((item) =>
			item.kind === "userMessage" ? [item.text] : [],
		);
		for (const text of prompts) {
			if (texts[texts.length - 1] !== text) texts.push(text);
		}
		for (const text of this.sentTexts) {
			if (!texts.includes(text)) texts.push(text);
		}
		return texts;
	});

	constructor(
		dial: (url: string, handlers: RpcHandlers) => RpcPort,
		applyThemeSetting: (setting: ThemeSetting) => void = applyThemeToPage,
	) {
		this.dial = dial;
		this.applyThemeSetting = applyThemeSetting;
		this.effects = {
			threadStarted: (notification) => {
				this.upsertThread(notification.thread);
			},
			threadStatus: (notification) => {
				const seqs = acceptThreadStatus(
					this.statusSeq,
					notification.threadId,
					notification.seq,
				);
				if (seqs === null) return;
				this.statusSeq = seqs;
				this.threads = this.threads.map((thread) =>
					thread.id === notification.threadId
						? {
								...thread,
								status: notification.status,
								activeFlags: notification.activeFlags,
							}
						: thread,
				);
			},
			retryWait: (notification) => {
				this.retryWait =
					notification.remainingMs > 0
						? {
								threadId: notification.threadId,
								turnId: notification.turnId,
								remainingMs: notification.remainingMs,
							}
						: null;
			},
			tokenUsage: (notification) => {
				this.usage = {
					...this.usage,
					[notification.threadId]: notification.usage,
				};
			},
			threadName: (notification) => {
				this.threads = this.threads.map((thread) =>
					thread.id === notification.threadId
						? { ...thread, title: notification.name }
						: thread,
				);
			},
			advisorNote: (notification) => {
				if (notification.threadId === this.activeThreadId) {
					void this.loadAdvisor(notification.threadId);
				}
			},
			requestResolved: (notification) => {
				const request = this.requests.find(
					(entry) => entry.id === notification.requestId,
				);
				if (request === undefined) return;
				this.settleRequest(request);
				const text = resolutionNotice(notification.resolution);
				if (text !== null) this.notify("info", text);
			},
			loginCompleted: (notification) => {
				this.loginCompleted(notification);
			},
			fileChangesUpdated: (notification) => {
				this.patchChanges = recordPatchChanges(
					this.patchChanges,
					notification.threadId,
					notification.itemId,
					notification.changes,
				);
			},
		};
	}

	start(location: SessionLocation): void {
		this.location = location;
		void this.connect();
	}

	stop(): void {
		this.connectionGeneration += 1;
		this.client?.close();
	}

	notify(
		tone: NoticeTone,
		text: string,
		actions: readonly NoticeAction[] = [],
	): void {
		const id = ++this.noticeSeq;
		const kept = this.notices.filter((notice) => notice.text !== text);
		this.notices = [...kept, { id, tone, text, actions }].slice(-MAX_NOTICES);
	}

	dismiss(id: number): void {
		this.notices = this.notices.filter((notice) => notice.id !== id);
	}

	openPanel(kind: PanelKind): void {
		if (this.panel === "login" && kind !== "login") this.cancelLogin();
		this.panel = kind;
		if (kind === "login") {
			this.loadLoginProviders().catch((error: unknown) =>
				this.reportFailure(
					"Unable to load login providers",
					error instanceof Error ? error : String(error),
					() => this.openPanel("login"),
				),
			);
		}
	}

	closePanel(): void {
		if (this.panel === "login") this.cancelLogin();
		this.panel = null;
	}

	private clearErrors(): void {
		this.notices = this.notices.filter((notice) => notice.tone !== "error");
	}

	async connect(): Promise<void> {
		const generation = ++this.connectionGeneration;
		this.client?.close();
		this.requests = [];
		this.clearErrors();
		const hashed = tokenFromHash(this.location.hash);
		if (hashed !== "") {
			this.accessToken = hashed;
		}
		if (this.location.hash !== "") {
			history.replaceState(
				null,
				"",
				`${this.location.pathname}${this.location.search}`,
			);
		}
		if (this.accessToken === "") {
			this.connection = "disconnected";
			this.notify(
				"error",
				"The URL has no access token. Open the full URL printed by dalgona web.",
			);
			return;
		}
		const rpc = this.dial(this.websocketUrl(), {
			onNotification: (notification) => {
				if (generation === this.connectionGeneration)
					this.handleNotification(notification);
			},
			onRequest: (request) => {
				if (generation === this.connectionGeneration) {
					this.requests = trackRequest(this.requests, request);
				}
			},
			onQuestion: (request) => {
				if (generation === this.connectionGeneration) {
					this.requests = trackRequest(this.requests, request);
				}
			},
			onStateChange: (state) => {
				if (generation !== this.connectionGeneration) return;
				const dropped =
					this.connection === "connected" && state === "disconnected";
				this.connection = state;
				if (dropped) {
					this.notify("error", "Disconnected from dalgona.", [
						this.reconnectAction(),
					]);
				}
			},
			onError: (message) => {
				if (generation === this.connectionGeneration) {
					this.notify("error", `Connection error: ${message}.`, [
						this.reconnectAction(),
					]);
				}
			},
		});
		this.client = rpc;
		this.selectionGeneration += 1;
		try {
			await rpc.open();
			if (generation !== this.connectionGeneration) return;
			const info = await rpc.call(
				"initialize",
				{
					clientInfo: {
						name: "dalgona-web",
						title: "dalgona web",
						version: "0.1.0",
					},
					capabilities: null,
				},
				parseInitializeResponse,
			);
			this.userAgent = info.userAgent;
			rpc.notify("initialized", {});
			await Promise.all([
				this.loadThreads(generation).catch((error) => {
					if (generation === this.connectionGeneration)
						this.reportFailure(
							"Unable to load threads",
							error instanceof Error ? error : String(error),
							() => void this.loadThreads(generation),
						);
				}),
				this.loadModels(generation).catch((error) => {
					if (generation === this.connectionGeneration)
						this.reportFailure(
							"Unable to load models",
							error instanceof Error ? error : String(error),
							() => void this.loadModels(generation),
						);
				}),
				this.loadConfig(generation).catch((error) => {
					if (generation === this.connectionGeneration)
						this.reportFailure(
							"Unable to load configuration",
							error instanceof Error ? error : String(error),
							() => void this.loadConfig(generation),
						);
				}),
				this.loadKeywords(generation).catch((error) => {
					if (generation === this.connectionGeneration)
						this.reportFailure(
							"Unable to load keywords",
							error instanceof Error ? error : String(error),
							() => void this.loadKeywords(generation),
						);
				}),
				this.loadSettings(generation).catch((error) => {
					if (generation === this.connectionGeneration)
						this.reportFailure(
							"Unable to load settings",
							error instanceof Error ? error : String(error),
							() => void this.loadSettings(generation),
						);
				}),
			]);
		} catch (error) {
			if (generation === this.connectionGeneration) {
				this.connection = "disconnected";
				const detail = error instanceof Error ? error.message : String(error);
				this.notify(
					"error",
					`Unable to connect to dalgona: ${detail}. Check that dalgona web is still running.`,
					[this.reconnectAction()],
				);
			}
		}
	}

	private websocketUrl(): string {
		return rpcSocketUrl(
			this.location.host,
			this.location.protocol === "https:",
			this.accessToken,
		);
	}

	private requireClient(): RpcPort {
		if (this.client === undefined) throw new Error("rpc client is not ready");
		return this.client;
	}

	private reconnectAction(): NoticeAction {
		return { label: "Reconnect", run: () => void this.connect() };
	}

	private reportFailure(
		action: string,
		error: Error | string,
		retry: () => void,
	): void {
		const detail = typeof error === "string" ? error : error.message;
		const sentence = detail.endsWith(".") ? detail : `${detail}.`;
		const recovery: NoticeAction =
			this.connection === "connected"
				? { label: "Try again", run: retry }
				: this.reconnectAction();
		this.notify("error", `${action}: ${sentence}`, [recovery]);
	}

	private async loadThreads(generation: number): Promise<void> {
		this.loadingThreads = true;
		try {
			const result = await this.requireClient().call(
				"thread/list",
				{ limit: 100, sortKey: "updated_at", sortDirection: "desc" },
				parseThreadListResponse,
			);
			if (generation !== this.connectionGeneration) return;
			this.threads = result.threads;
			if (this.activeThreadId !== null) {
				await this.selectThread(this.activeThreadId);
				return;
			}
			const first = result.threads[0];
			if (first !== undefined) await this.selectThread(first.id);
		} finally {
			if (generation === this.connectionGeneration) this.loadingThreads = false;
		}
	}

	private async loadModels(generation: number): Promise<void> {
		const result = await this.requireClient().call(
			"model/list",
			{ includeHidden: false },
			parseModelListResponse,
		);
		if (generation !== this.connectionGeneration) return;
		this.models = result;
		const selected = result.find((model) => model.id === this.selectedModel);
		const first = result.find((model) => !model.hidden);
		if (selected === undefined && first !== undefined)
			this.selectedModel = first.id;
	}

	private async loadConfig(generation: number): Promise<void> {
		const result = await this.requireClient().call(
			"config/read",
			{},
			parseConfigReadResponse,
		);
		if (generation !== this.connectionGeneration) return;
		this.status = result;
		if (result.model) this.selectedModel = result.model;
	}

	async readItemOutput(itemId: string, offset: number): Promise<OutputPage> {
		const threadId = this.activeThreadId;
		if (threadId === null) throw new Error("No thread is open.");
		return this.requireClient().call(
			"dalgona/item/output/read",
			{ threadId, itemId, offset },
			parseOutputReadResponse,
		);
	}

	private async loadSettings(generation: number): Promise<void> {
		const settings = await this.requireClient().call(
			"dalgona/settings/read",
			{},
			parseSettingsReadResponse,
		);
		if (generation !== this.connectionGeneration) return;
		this.settingValues = settings;
		const theme = settings.find((setting) => setting.key === "theme");
		if (theme !== undefined)
			this.applyThemeSetting(parseThemeSetting(theme.value));
		const verbosity = settings.find((setting) => setting.key === "verbosity");
		if (verbosity !== undefined)
			this.verbosity = parseVerbosity(verbosity.value);
		const hidden = settings.find((setting) => setting.key === "statusHide");
		if (hidden !== undefined && Array.isArray(hidden.value))
			this.statusHide = hidden.value.filter(
				(item): item is string => typeof item === "string",
			);
	}

	private async loadKeywords(generation: number): Promise<void> {
		const result = await this.requireClient().call(
			"dalgona/keywords/list",
			{},
			parseKeywordsResponse,
		);
		if (generation !== this.connectionGeneration) return;
		this.keywords = result;
	}

	async matchKeywords(text: string): Promise<readonly KeywordMatch[]> {
		const params: JsonValue =
			this.activeThreadId === null
				? { text, kind: "prompt" }
				: { threadId: this.activeThreadId, text, kind: "prompt" };
		return this.requireClient().call(
			"dalgona/keywords/match",
			params,
			parseKeywordMatches,
		);
	}

	private upsertThread(thread: ThreadDetail): void {
		this.completedThreadIds = recordThreadTurn(
			this.completedThreadIds,
			thread.summary.id,
			lastTurnStatus(thread),
		);
		const summary = thread.summary;
		const index = this.threads.findIndex((entry) => entry.id === summary.id);
		if (index < 0) {
			this.threads = [summary, ...this.threads];
			return;
		}
		const next = this.threads.slice();
		next[index] = summary;
		this.threads = next;
	}

	private async loadAdvisor(threadId: string): Promise<void> {
		try {
			const advisors = await this.requireClient().call(
				"dalgona/advisor/status",
				{ threadId },
				parseAdvisorStatusResponse,
			);
			if (threadId === this.activeThreadId)
				this.advisor = { kind: "loaded", advisors };
		} catch (error) {
			if (threadId !== this.activeThreadId) return;
			this.advisor =
				error instanceof RpcError && error.code === -32001
					? { kind: "off" }
					: { kind: "unavailable" };
		}
	}

	async selectThread(threadId: string): Promise<void> {
		const selection = ++this.selectionGeneration;
		this.loadingTranscript = true;
		try {
			const result = await this.requireClient().call(
				"thread/resume",
				{ threadId },
				parseThreadStartResponse,
			);
			if (selection !== this.selectionGeneration) return;
			this.upsertThread(result.thread);
			if (threadId !== this.activeThreadId) {
				this.queuedFollowups = [];
				this.patchChanges = keepPendingPatchChanges(
					this.patchChanges,
					this.requests,
				);
			}
			this.activeThreadId = threadId;
			this.transcript = transcriptForThread(result.thread, this.knownChildren);
			this.advisor = { kind: "unknown" };
			this.clearErrors();
			this.status = result.status;
			if (result.usage !== null)
				this.usage = { ...this.usage, [threadId]: result.usage };
			this.selectedModel = result.status.model || this.selectedModel;
			void this.loadAdvisor(threadId);
			if (result.thread.turns.length === 0)
				await this.readThread(threadId, selection);
		} catch (error) {
			if (selection === this.selectionGeneration)
				this.reportFailure(
					"Unable to open thread",
					error instanceof Error ? error : String(error),
					() => void this.selectThread(threadId),
				);
		} finally {
			if (selection === this.selectionGeneration)
				this.loadingTranscript = false;
		}
	}

	private async readThread(threadId: string, selection: number): Promise<void> {
		const thread = await this.requireClient().call(
			"thread/read",
			{ threadId, includeTurns: true },
			parseThreadReadResponse,
		);
		this.completedThreadIds = recordThreadTurn(
			this.completedThreadIds,
			thread.summary.id,
			lastTurnStatus(thread),
		);
		if (selection === this.selectionGeneration)
			this.transcript = transcriptForThread(thread, this.knownChildren);
	}

	async createThread(): Promise<void> {
		const selection = ++this.selectionGeneration;
		this.startingThread = true;
		try {
			const result = await this.requireClient().call(
				"thread/start",
				{ model: this.selectedModel || null },
				parseThreadStartResponse,
			);
			if (selection !== this.selectionGeneration) return;
			this.loadingTranscript = false;
			this.upsertThread(result.thread);
			this.activeThreadId = result.thread.summary.id;
			this.transcript = transcriptForThread(result.thread);
			this.status = result.status;
			if (result.usage !== null)
				this.usage = {
					...this.usage,
					[result.thread.summary.id]: result.usage,
				};
			this.selectedModel = result.status.model || this.selectedModel;
			this.queuedFollowups = [];
			this.clearErrors();
			void this.loadAdvisor(result.thread.summary.id);
		} catch (error) {
			this.reportFailure(
				"Unable to start thread",
				error instanceof Error ? error : String(error),
				() => void this.createThread(),
			);
		} finally {
			this.startingThread = false;
		}
	}

	async sendMessage(text: string): Promise<boolean> {
		const threadId = this.activeThreadId;
		if (threadId === null) return false;
		try {
			const result = await this.requireClient().call(
				"turn/start",
				{
					threadId,
					input: [{ type: "text", text, text_elements: [] }],
					model: this.selectedModel || null,
				},
				parseTurnStartResponse,
			);
			const started = applyOpenThreadNotification(
				{
					transcript: this.transcript,
					knownChildren: this.knownChildren,
					activeThreadId: this.activeThreadId,
				},
				{
					type: "turnStarted",
					threadId,
					turn: result.turn,
				},
			);
			this.transcript = started.transcript;
			this.recordPreview(threadId, text);
			this.sentTexts = [...this.sentTexts, text];
			return true;
		} catch (error) {
			this.reportFailure(
				"Unable to send message",
				error instanceof Error ? error : String(error),
				() => void this.sendMessage(text),
			);
			return false;
		}
	}

	private recordPreview(threadId: string, text: string): void {
		this.threads = this.threads.map((thread) =>
			thread.id === threadId && thread.preview === ""
				? { ...thread, preview: text }
				: thread,
		);
	}

	async steerMessage(text: string): Promise<void> {
		const threadId = this.activeThreadId;
		const turnId = this.transcript.turnId;
		if (threadId === null || turnId === null) return;
		try {
			await this.requireClient().call(
				"turn/steer",
				turnSteerParams(threadId, turnId, text),
				parseTurnSteerResponse,
			);
			this.sentTexts = [...this.sentTexts, text];
		} catch (error) {
			this.reportFailure(
				"Unable to steer turn",
				error instanceof Error ? error : String(error),
				() => void this.steerMessage(text),
			);
		}
	}

	queueFollowup(text: string): void {
		if (!this.running) {
			void this.sendMessage(text);
			return;
		}
		this.queuedFollowups = [...this.queuedFollowups, text];
	}

	editFollowup(index: number): string | undefined {
		const text = this.queuedFollowups[index];
		if (text === undefined) return undefined;
		this.removeFollowup(index);
		return text;
	}

	removeFollowup(index: number): void {
		this.queuedFollowups = this.queuedFollowups.filter(
			(_, position) => position !== index,
		);
	}

	async interruptTurn(): Promise<void> {
		if (this.activeThreadId === null || this.transcript.turnId === null) return;
		try {
			await this.requireClient().call(
				"turn/interrupt",
				{ threadId: this.activeThreadId, turnId: this.transcript.turnId },
				parseVoidResponse,
			);
		} catch (error) {
			this.reportFailure(
				"Unable to interrupt turn",
				error instanceof Error ? error : String(error),
				() => void this.interruptTurn(),
			);
		}
	}

	async changeModel(model: string): Promise<void> {
		this.selectedModel = model;
		this.status = { ...this.status, model };
		try {
			await this.requireClient().call(
				"config/value/write",
				{ keyPath: "model", value: model, mergeStrategy: "replace" },
				parseVoidResponse,
			);
		} catch (error) {
			this.reportFailure(
				"Unable to save model selection",
				error instanceof Error ? error : String(error),
				() => void this.changeModel(model),
			);
		}
	}

	async saveSettings(edits: readonly SettingEdit[]): Promise<boolean> {
		const generation = this.connectionGeneration;
		try {
			await this.requireClient().call(
				"config/batchWrite",
				{
					edits: edits.map((edit) => ({
						keyPath: edit.keyPath,
						value: edit.value,
						mergeStrategy: "replace",
					})),
				},
				parseVoidResponse,
			);
		} catch (error) {
			this.reportFailure(
				"Unable to save settings",
				error instanceof Error ? error : String(error),
				() => void this.saveSettings(edits),
			);
			return false;
		}
		const theme = edits.find((edit) => edit.keyPath === "theme");
		if (typeof theme?.value === "string")
			this.applyThemeSetting(parseThemeSetting(theme.value));
		try {
			await this.loadSettings(generation);
		} catch (error) {
			this.reportFailure(
				"Unable to load settings",
				error instanceof Error ? error : String(error),
				() => void this.loadSettings(generation),
			);
		}
		if (edits.some((edit) => edit.keyPath === "approval"))
			this.notify(
				"info",
				"saved; open threads keep their approval and sandbox",
			);
		this.closePanel();
		return true;
	}

	async loadLoginProviders(): Promise<void> {
		const generation = this.connectionGeneration;
		const providers = await this.requireClient().call(
			"dalgona/account/providers/list",
			{},
			parseLoginProvidersResponse,
		);
		if (generation !== this.connectionGeneration) return;
		this.loginProviders = providers;
	}

	async startLogin(
		provider: string,
		method: LoginMethod,
		apiKey = "",
	): Promise<void> {
		const flow: LoginFlow = {
			provider,
			method,
			loginId: null,
			authUrl: null,
			verificationUrl: null,
			userCode: null,
			error: null,
			pending: true,
		};
		this.login = flow;
		try {
			const start = await this.requireClient().call(
				"account/login/start",
				this.loginStartParams(method, provider, apiKey),
				parseLoginStartResponse,
			);
			if (start.type === "apiKey") {
				if (this.login === flow) this.login = { ...flow, pending: false };
				return;
			}
			if (this.login !== flow) {
				this.sendLoginCancel(start.loginId);
				return;
			}
			this.login = {
				...flow,
				pending: false,
				loginId: start.loginId,
				authUrl: start.type === "chatgpt" ? start.authUrl : null,
				verificationUrl:
					start.type === "chatgptDeviceCode" ? start.verificationUrl : null,
				userCode: start.type === "chatgptDeviceCode" ? start.userCode : null,
			};
		} catch (error) {
			if (this.login !== flow) return;
			const text = error instanceof Error ? error.message : String(error);
			if (method === "apiKey") {
				this.login = { ...flow, pending: false, error: text };
				return;
			}
			this.login = null;
			this.notify("error", `Login failed: ${text}; run /login again`, [
				{
					label: "Try again",
					run: () => void this.startLogin(provider, method),
				},
			]);
		}
	}

	cancelLogin(): void {
		const loginId = this.login?.loginId ?? null;
		this.login = null;
		if (loginId !== null) this.sendLoginCancel(loginId);
	}

	private sendLoginCancel(loginId: string): void {
		this.client
			?.call("account/login/cancel", { loginId }, parseVoidResponse)
			.catch(() => undefined);
	}

	private loginCompleted(
		notification: Extract<AppNotification, { readonly type: "loginCompleted" }>,
	): void {
		const flow = this.login;
		if (flow === null) return;
		const ours =
			notification.loginId !== null
				? notification.loginId === flow.loginId
				: notification.provider === flow.provider;
		if (!ours) return;
		if (!notification.success) {
			if (flow.method === "apiKey") {
				this.login = {
					...flow,
					pending: false,
					error: notification.error ?? "login failed",
				};
				return;
			}
			this.login = null;
			this.notify(
				"error",
				`Login failed: ${notification.error ?? "unknown error"}; run /login again`,
				[
					{
						label: "Try again",
						run: () => void this.startLogin(flow.provider, flow.method),
					},
				],
			);
			return;
		}
		this.login = null;
		this.reloadModels();
		this.notify("info", `Signed in to ${flow.provider}`);
		if (this.panel === "login") this.closePanel();
	}

	private reloadModels(): void {
		this.loadModels(this.connectionGeneration).catch((error: unknown) =>
			this.reportFailure(
				"Unable to load models",
				error instanceof Error ? error : String(error),
				() => this.reloadModels(),
			),
		);
	}

	private loginStartParams(
		method: LoginMethod,
		provider: string,
		apiKey: string,
	): JsonValue {
		if (method === "apiKey") return { type: "apiKey", provider, apiKey };
		return { type: method === "browser" ? "chatgpt" : "chatgptDeviceCode" };
	}

	respondApproval(request: ApprovalRequest, decision: Decision): void {
		this.requireClient().respond(request.id, { decision });
		this.settleRequest(request);
	}

	answerQuestion(
		request: QuestionRequest,
		selections: readonly QuestionSelection[],
	): void {
		this.requireClient().respond(
			request.id,
			questionResponse(request, selections),
		);
		this.settleRequest(request);
	}

	cancelQuestion(request: QuestionRequest): void {
		this.requireClient().respond(request.id, { answers: {} });
		this.settleRequest(request);
	}

	private settleRequest(request: ServerRequest): void {
		this.requests = resolveRequest(this.requests, request.id);
		this.patchChanges = forgetPatchChanges(this.patchChanges, request);
	}

	private handleNotification(notification: AppNotification): void {
		const update = applyOpenThreadNotification(
			{
				transcript: this.transcript,
				knownChildren: this.knownChildren,
				activeThreadId: this.activeThreadId,
			},
			notification,
		);
		if (update.transcript !== this.transcript) this.transcriptRevision += 1;
		this.transcript = update.transcript;
		this.knownChildren = update.knownChildren;
		if (
			notification.type === "turnStarted" ||
			notification.type === "turnCompleted"
		) {
			this.completedThreadIds = recordThreadTurn(
				this.completedThreadIds,
				notification.threadId,
				notification.turn.status,
			);
		}
		if (isEffectKind(notification.type)) {
			const effect = this.effects[notification.type] as (
				value: AppNotification,
			) => void;
			effect(notification);
		}
		if (update.sendFollowup && this.activeThreadId !== null) {
			void this.sendNextFollowup(this.activeThreadId);
		}
	}

	private sendNextFollowup(threadId: string): void {
		const next = this.queuedFollowups[0];
		if (next === undefined || this.activeThreadId !== threadId) return;
		this.queuedFollowups = dropSentFollowup(this.queuedFollowups);
		void this.sendMessage(next);
	}
}
