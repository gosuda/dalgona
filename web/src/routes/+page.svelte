<script lang="ts">
import { onMount } from "svelte";
import "../app.css";
import ApprovalDialog from "#lib/components/ApprovalDialog.svelte";
import Composer from "#lib/components/Composer.svelte";
import ModelPicker from "#lib/components/ModelPicker.svelte";
import StatusBar from "#lib/components/StatusBar.svelte";
import ThreadList from "#lib/components/ThreadList.svelte";
import Transcript from "#lib/components/Transcript.svelte";
import { threadTitle } from "#lib/labels.ts";
import { RpcClient, RpcError } from "#lib/protocol.ts";
import {
	emptyTranscript,
	itemsAwaitingApproval,
	reduceNotification,
	resolveApprovalRequest,
	trackApprovalRequest,
	transcriptForThread,
} from "#lib/reducer.ts";
import { turnSteerParams } from "#lib/requests.ts";
import type {
	AdvisorSummary,
	AppNotification,
	ApprovalRequest,
	ConnectionState,
	KeywordDefinition,
	ModelOption,
	StatusInfo,
	ThreadDetail,
	ThreadSummary,
	TranscriptState,
} from "#lib/types.ts";
import {
	parseAdvisorStatusResponse,
	parseConfigReadResponse,
	parseInitializeResponse,
	parseKeywordsResponse,
	parseModelListResponse,
	parseThreadListResponse,
	parseThreadReadResponse,
	parseThreadStartResponse,
	parseTurnStartResponse,
	parseTurnSteerResponse,
	parseVoidResponse,
} from "#lib/validation.ts";

let connection = $state<ConnectionState>("idle");
let threads = $state<readonly ThreadSummary[]>([]);
let activeThreadId = $state<string | null>(null);
let transcript = $state<TranscriptState>(emptyTranscript(null));
let models = $state<readonly ModelOption[]>([]);
let selectedModel = $state("");
let status = $state<StatusInfo>({
	model: "Default",
	approval: "auto",
	sandbox: "workspace-write",
	runner: "local",
});
let keywords = $state<readonly KeywordDefinition[]>([]);
let advisor = $state<AdvisorSummary>({ kind: "unknown" });
let approvals = $state<readonly ApprovalRequest[]>([]);
let queuedFollowups = $state<readonly string[]>([]);
let loadingThreads = $state(false);
let loadingTranscript = $state(false);
let startingThread = $state(false);
let errorMessage = $state("");
let client: RpcClient | undefined;
let connectionGeneration = 0;
let selectionGeneration = 0;

let running = $derived(transcript.turnStatus === "inProgress");
let currentApproval = $derived(approvals[0] ?? null);
let awaitingApproval = $derived(
	itemsAwaitingApproval(approvals, activeThreadId),
);
let approvalChanges = $derived.by(() => {
	const item = transcript.items.find(
		(entry) => entry.id === currentApproval?.itemId,
	);
	return item?.kind === "fileChange" ? item.changes : [];
});
let activeTitle = $derived.by(() => {
	const thread = threads.find((entry) => entry.id === activeThreadId);
	return thread === undefined ? "No thread selected" : threadTitle(thread);
});

onMount(() => {
	void connect();
	return () => {
		connectionGeneration += 1;
		client?.close();
	};
});

function requireClient(): RpcClient {
	if (client === undefined) throw new Error("rpc client is not ready");
	return client;
}

function websocketUrl(): string {
	const scheme = location.protocol === "https:" ? "wss" : "ws";
	const token = new URLSearchParams(location.hash.slice(1)).get("token") ?? "";
	return `${scheme}://${location.host}/rpc?token=${encodeURIComponent(token)}`;
}

async function connect(): Promise<void> {
	const generation = ++connectionGeneration;
	client?.close();
	const token = new URLSearchParams(location.hash.slice(1)).get("token") ?? "";
	if (token === "") {
		connection = "disconnected";
		errorMessage =
			"The URL has no access token. Open the full URL printed by dalgona web.";
		return;
	}
	const rpc = new RpcClient(websocketUrl(), {
		onNotification: (notification) => {
			if (generation === connectionGeneration) handleNotification(notification);
		},
		onRequest: (request) => {
			if (generation === connectionGeneration) {
				approvals = trackApprovalRequest(approvals, request);
			}
		},
		onStateChange: (state) => {
			if (generation === connectionGeneration) connection = state;
		},
		onError: (message) => {
			if (generation === connectionGeneration) {
				errorMessage = `${message}. Reconnect and try again.`;
			}
		},
	});
	client = rpc;
	try {
		await rpc.open();
		if (generation !== connectionGeneration) return;
		await rpc.call(
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
		rpc.notify("initialized", {});
		await Promise.all([
			loadThreads().catch((error) =>
				reportFailure(
					"Unable to load threads",
					error instanceof Error ? error : String(error),
				),
			),
			loadModels().catch((error) =>
				reportFailure(
					"Unable to load models",
					error instanceof Error ? error : String(error),
				),
			),
			loadConfig().catch((error) =>
				reportFailure(
					"Unable to load configuration",
					error instanceof Error ? error : String(error),
				),
			),
			loadKeywords().catch((error) =>
				reportFailure(
					"Unable to load keywords",
					error instanceof Error ? error : String(error),
				),
			),
		]);
	} catch (error) {
		if (generation === connectionGeneration) {
			connection = "disconnected";
			const detail = error instanceof Error ? error.message : String(error);
			errorMessage = `Unable to connect to dalgona: ${detail}. Check that dalgona web is still running, then select Reconnect.`;
		}
	}
}

function reportFailure(action: string, error: Error | string): void {
	const detail = typeof error === "string" ? error : error.message;
	errorMessage = `${action}: ${detail}. Reconnect and try again.`;
}

async function loadThreads(): Promise<void> {
	loadingThreads = true;
	try {
		const result = await requireClient().call(
			"thread/list",
			{ limit: 100, sortKey: "updated_at", sortDirection: "desc" },
			parseThreadListResponse,
		);
		threads = result.threads;
		const first = result.threads[0];
		if (activeThreadId === null && first !== undefined)
			await selectThread(first.id);
	} finally {
		loadingThreads = false;
	}
}

async function loadModels(): Promise<void> {
	const result = await requireClient().call(
		"model/list",
		{ includeHidden: false },
		parseModelListResponse,
	);
	models = result;
	const selected = result.find((model) => model.id === selectedModel);
	const first = result.find((model) => !model.hidden);
	if (selected === undefined && first !== undefined) selectedModel = first.id;
}

async function loadConfig(): Promise<void> {
	const result = await requireClient().call(
		"config/read",
		{},
		parseConfigReadResponse,
	);
	status = result;
	if (result.model) selectedModel = result.model;
}

async function loadKeywords(): Promise<void> {
	keywords = await requireClient().call(
		"dalgona/keywords/list",
		{},
		parseKeywordsResponse,
	);
}

function upsertThread(thread: ThreadDetail): void {
	const summary = thread.summary;
	const index = threads.findIndex((entry) => entry.id === summary.id);
	if (index < 0) {
		threads = [summary, ...threads];
		return;
	}
	const next = threads.slice();
	next[index] = summary;
	threads = next;
}

async function loadAdvisor(threadId: string): Promise<void> {
	try {
		const advisors = await requireClient().call(
			"dalgona/advisor/status",
			{ threadId },
			parseAdvisorStatusResponse,
		);
		if (threadId === activeThreadId) advisor = { kind: "loaded", advisors };
	} catch (error) {
		if (threadId !== activeThreadId) return;
		advisor =
			error instanceof RpcError && error.code === -32001
				? { kind: "off" }
				: { kind: "unavailable" };
	}
}

async function selectThread(threadId: string): Promise<void> {
	const selection = ++selectionGeneration;
	loadingTranscript = true;
	activeThreadId = threadId;
	transcript = emptyTranscript(threadId);
	advisor = { kind: "unknown" };
	queuedFollowups = [];
	try {
		const result = await requireClient().call(
			"thread/resume",
			{ threadId },
			parseThreadStartResponse,
		);
		if (selection !== selectionGeneration) return;
		upsertThread(result.thread);
		transcript = transcriptForThread(result.thread);
		status = result.status;
		selectedModel = result.status.model || selectedModel;
		void loadAdvisor(threadId);
		if (result.thread.turns.length === 0) await readThread(threadId, selection);
	} catch (error) {
		if (selection === selectionGeneration)
			reportFailure(
				"Unable to open thread",
				error instanceof Error ? error : String(error),
			);
	} finally {
		if (selection === selectionGeneration) loadingTranscript = false;
	}
}

async function readThread(threadId: string, selection: number): Promise<void> {
	const thread = await requireClient().call(
		"thread/read",
		{ threadId, includeTurns: true },
		parseThreadReadResponse,
	);
	if (selection === selectionGeneration)
		transcript = transcriptForThread(thread);
}

async function createThread(): Promise<void> {
	const selection = ++selectionGeneration;
	startingThread = true;
	try {
		const result = await requireClient().call(
			"thread/start",
			{ model: selectedModel || null },
			parseThreadStartResponse,
		);
		if (selection !== selectionGeneration) return;
		loadingTranscript = false;
		upsertThread(result.thread);
		activeThreadId = result.thread.summary.id;
		transcript = transcriptForThread(result.thread);
		status = result.status;
		selectedModel = result.status.model || selectedModel;
		queuedFollowups = [];
		errorMessage = "";
		void loadAdvisor(result.thread.summary.id);
	} catch (error) {
		reportFailure(
			"Unable to start thread",
			error instanceof Error ? error : String(error),
		);
	} finally {
		startingThread = false;
	}
}

async function sendMessage(text: string): Promise<void> {
	const threadId = activeThreadId;
	if (threadId === null) return;
	try {
		const result = await requireClient().call(
			"turn/start",
			{
				threadId,
				input: [{ type: "text", text, text_elements: [] }],
				model: selectedModel || null,
			},
			parseTurnStartResponse,
		);
		transcript = reduceNotification(transcript, {
			type: "turnStarted",
			threadId,
			turn: result.turn,
		});
		recordPreview(threadId, text);
	} catch (error) {
		reportFailure(
			"Unable to send message",
			error instanceof Error ? error : String(error),
		);
	}
}

function recordPreview(threadId: string, text: string): void {
	threads = threads.map((thread) =>
		thread.id === threadId && thread.preview === ""
			? { ...thread, preview: text }
			: thread,
	);
}

async function steerMessage(text: string): Promise<void> {
	const threadId = activeThreadId;
	const turnId = transcript.turnId;
	if (threadId === null || turnId === null) return;
	try {
		await requireClient().call(
			"turn/steer",
			turnSteerParams(threadId, turnId, text),
			parseTurnSteerResponse,
		);
	} catch (error) {
		reportFailure(
			"Unable to steer turn",
			error instanceof Error ? error : String(error),
		);
	}
}

function queueFollowup(text: string): void {
	if (!running) {
		void sendMessage(text);
		return;
	}
	queuedFollowups = [...queuedFollowups, text];
}

async function interruptTurn(): Promise<void> {
	if (activeThreadId === null || transcript.turnId === null) return;
	try {
		await requireClient().call(
			"turn/interrupt",
			{ threadId: activeThreadId, turnId: transcript.turnId },
			parseVoidResponse,
		);
	} catch (error) {
		reportFailure(
			"Unable to interrupt turn",
			error instanceof Error ? error : String(error),
		);
	}
}

async function changeModel(model: string): Promise<void> {
	selectedModel = model;
	status = { ...status, model };
	try {
		await requireClient().call(
			"config/value/write",
			{ keyPath: "model", value: model, mergeStrategy: "replace" },
			parseVoidResponse,
		);
	} catch (error) {
		reportFailure(
			"Unable to save model selection",
			error instanceof Error ? error : String(error),
		);
	}
}

function respondApproval(
	request: ApprovalRequest,
	decision: "accept" | "acceptForSession" | "decline" | "cancel",
): void {
	requireClient().respond(request.id, { decision });
	approvals = resolveApprovalRequest(approvals, request.id);
}

function handleNotification(notification: AppNotification): void {
	switch (notification.type) {
		case "threadStarted":
			upsertThread(notification.thread);
			transcript = reduceNotification(transcript, notification);
			break;
		case "threadStatus":
			threads = threads.map((thread) =>
				thread.id === notification.threadId
					? { ...thread, status: notification.status }
					: thread,
			);
			break;
		case "threadName":
			threads = threads.map((thread) =>
				thread.id === notification.threadId
					? { ...thread, title: notification.name }
					: thread,
			);
			break;
		case "plan":
		case "turnStarted":
		case "turnCompleted":
		case "itemStarted":
		case "itemCompleted":
		case "delta":
		case "error":
			transcript = reduceNotification(transcript, notification);
			if (
				notification.type === "turnCompleted" &&
				notification.threadId === activeThreadId &&
				notification.turn.status === "completed"
			) {
				void sendNextFollowup(notification.threadId);
			}
			break;
		case "advisorNote":
			if (notification.threadId === activeThreadId) {
				void loadAdvisor(notification.threadId);
			}
			break;
		case "requestResolved":
			approvals = resolveApprovalRequest(approvals, notification.requestId);
			break;
		default: {
			const exhaustive: never = notification;
			throw new Error(`unsupported notification: ${String(exhaustive)}`);
		}
	}
}

async function sendNextFollowup(threadId: string): Promise<void> {
	const next = queuedFollowups[0];
	if (next === undefined || activeThreadId !== threadId) return;
	queuedFollowups = queuedFollowups.slice(1);
	await sendMessage(next);
}
</script>

<svelte:head>
	<title>{activeTitle} · dalgona</title>
</svelte:head>

<a class="skip-link" href="#main-content">Skip to transcript</a>

<div class="shell">
	<ThreadList
		{threads}
		activeId={activeThreadId}
		loading={loadingThreads}
		connected={connection === "connected"}
		disabled={connection !== "connected" || startingThread}
		onNew={() => void createThread()}
		onSelect={(threadId) => void selectThread(threadId)}
	/>

	<main id="main-content" class="workspace" tabindex="-1">
		<StatusBar {status} {connection} {advisor} turnStatus={transcript.turnStatus} onReconnect={() => void connect()} />

		<div class="workspace-heading">
			<div>
				<p>Current thread</p>
				<h2>{activeTitle}</h2>
			</div>
			<ModelPicker
				{models}
				value={selectedModel}
				disabled={connection !== "connected"}
				onChange={(model) => void changeModel(model)}
			/>
		</div>

		{#if errorMessage}
			<div class="error-banner" role="alert">
				<p>{errorMessage}</p>
				<button class="quiet" onclick={() => (errorMessage = "")}>Dismiss error</button>
			</div>
		{/if}

		<Transcript
			threadId={activeThreadId}
			items={transcript.items}
			turnStatus={transcript.turnStatus}
			error={transcript.error}
			plan={transcript.plan}
			connected={connection === "connected"}
			{keywords}
			loading={loadingTranscript}
			{awaitingApproval}
		/>

		<Composer
			disabled={connection !== "connected" || activeThreadId === null || loadingTranscript}
			{running}
			queuedCount={queuedFollowups.length}
			{keywords}
			onSend={(text) => void sendMessage(text)}
			onSteer={(text) => void steerMessage(text)}
			onFollowup={queueFollowup}
			onInterrupt={() => void interruptTurn()}
		/>
	</main>
</div>

<ApprovalDialog
	request={currentApproval}
	changes={approvalChanges}
	onDecision={respondApproval}
/>

<style>
	.shell {
		display: grid;
		grid-template-columns: minmax(16rem, 19rem) minmax(0, 1fr);
		height: 100dvh;
		min-height: 0;
	}

	.workspace {
		display: flex;
		min-width: 0;
		min-height: 0;
		flex-direction: column;
		outline: none;
	}

	.workspace-heading {
		display: flex;
		align-items: end;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: var(--space-3);
		padding: var(--space-4) var(--space-5);
	}

	.workspace-heading p {
		margin: 0 0 var(--space-1);
		color: var(--color-text-secondary);
		font-size: var(--text-caption);
	}

	.workspace-heading h2 {
		max-width: 48rem;
		margin: 0;
		overflow-wrap: break-word;
		font-size: var(--text-heading);
		font-weight: 650;
		line-height: 1.25;
		text-wrap: balance;
	}

	.error-banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
		margin: 0 var(--space-5) var(--space-3);
		border-inline-start: 2px solid var(--color-danger);
		background: var(--color-danger-surface);
		padding: var(--space-3);
	}

	.error-banner p {
		margin: 0;
		font-size: var(--text-body-small);
	}

	.error-banner button {
		flex: none;
		min-height: 32px;
		padding: var(--space-1) var(--space-3);
		font-size: var(--text-caption);
	}

	:global(.transcript) {
		flex: 1;
	}

	@media (max-width: 52rem) {
		.shell {
			grid-template-columns: minmax(0, 1fr);
			grid-template-rows: auto auto;
			height: auto;
			min-height: 100dvh;
		}

		:global(.sidebar) {
			max-height: 18rem;
		}
	}

	@media (max-width: 40rem) {
		.workspace-heading {
			padding: var(--space-3) var(--space-4);
		}

		.error-banner {
			align-items: start;
			margin-inline: var(--space-4);
		}
	}
</style>
