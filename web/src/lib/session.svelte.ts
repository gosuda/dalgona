import { threadTitle } from "./labels";
import { RpcError, rpcSocketUrl, tokenFromHash } from "./protocol";
import {
	acceptThreadStatus,
	activeApproval,
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
	resolveApprovalRequest,
	trackApprovalRequest,
	transcriptForThread,
} from "./reducer";
import { turnSteerParams } from "./requests";
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
	ModelOption,
	Notice,
	NoticeAction,
	NoticeTone,
	OutputPage,
	RequestId,
	RetryWait,
	RpcHandlers,
	StatusInfo,
	ThreadDetail,
	ThreadSummary,
	TokenUsage,
	TranscriptState,
	WaitingElsewhere,
} from "./types";
import {
	parseAdvisorStatusResponse,
	parseConfigReadResponse,
	parseInitializeResponse,
	parseKeywordMatches,
	parseKeywordsResponse,
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
	approvals: readonly ApprovalRequest[] = $state([]);
	patchChanges: PatchChanges = $state({});
	knownChildren: KnownChildThreads = $state({});
	queuedFollowups: readonly string[] = $state([]);
	loadingThreads: boolean = $state(false);
	loadingTranscript: boolean = $state(false);
	startingThread: boolean = $state(false);
	notices: readonly Notice[] = $state([]);
	statusSeq: Readonly<Record<string, number>> = $state({});
	retryWait: RetryWait | null = $state(null);
	completedThreadIds: ReadonlySet<string> = $state(new Set());
	verbosity: Verbosity = $state("medium");
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
	currentApproval: ApprovalRequest | null = $derived(
		activeApproval(
			this.approvals,
			this.activeThreadId,
			this.transcript.childThreadIds,
		),
	);
	waitingElsewhere: WaitingElsewhere = $derived.by(() => {
		const waiting = requestsElsewhere(
			this.approvals,
			this.activeThreadId,
			this.transcript.childThreadIds,
		);
		return { count: waiting.length, threadId: waiting[0]?.threadId ?? null };
	});
	awaitingApproval: ReadonlySet<string> = $derived(
		itemsAwaitingApproval(
			this.approvals,
			this.activeThreadId,
			this.transcript.childThreadIds,
		),
	);
	approvalChanges: readonly FileChange[] = $derived.by(() => {
		const approval = this.currentApproval;
		if (approval === null) return [];
		const changes = patchChangesFor(
			this.patchChanges,
			approval.threadId,
			approval.itemId,
		);
		if (changes !== null) return changes;
		const item = this.transcript.items.find(
			(entry) => entry.id === approval.itemId,
		);
		return item?.kind === "fileChange" ? item.changes : [];
	});
	activeTitle: string = $derived.by(() => {
		const thread = this.threads.find(
			(entry) => entry.id === this.activeThreadId,
		);
		return thread === undefined ? "No thread selected" : threadTitle(thread);
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
				const approval = this.approvals.find(
					(entry) => entry.id === notification.requestId,
				);
				if (approval !== undefined) this.settleApproval(approval);
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

	private clearErrors(): void {
		this.notices = this.notices.filter((notice) => notice.tone !== "error");
	}

	async connect(): Promise<void> {
		const generation = ++this.connectionGeneration;
		this.client?.close();
		this.approvals = [];
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
					this.approvals = trackApprovalRequest(this.approvals, request);
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
					this.approvals,
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

	respondApproval(request: ApprovalRequest, decision: Decision): void {
		this.requireClient().respond(request.id, { decision });
		this.settleApproval(request);
	}

	private settleApproval(request: ApprovalRequest): void {
		this.approvals = resolveApprovalRequest(this.approvals, request.id);
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
