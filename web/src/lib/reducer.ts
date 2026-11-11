import type {
	AppNotification,
	ApprovalRequest,
	RequestId,
	ThreadDetail,
	TranscriptItem,
	TranscriptState,
	TurnStatus,
} from "./types";

export type KnownChildThreads = Readonly<Record<string, readonly string[]>>;

export function recordChildThread(
	known: KnownChildThreads,
	threadId: string,
	childId: string,
): KnownChildThreads {
	const existing: readonly string[] = known[threadId] ?? [];
	if (existing.includes(childId)) {
		return known;
	}
	return { ...known, [threadId]: [...existing, childId] };
}

export function emptyTranscript(threadId: string | null): TranscriptState {
	return {
		threadId,
		turnId: null,
		turnStatus: null,
		items: [],
		error: null,
		plan: null,
		childThreadIds: [],
	};
}

export function transcriptForThread(
	thread: ThreadDetail,
	knownChildren: KnownChildThreads = {},
): TranscriptState {
	const items: TranscriptItem[] = [];
	let turnId: string | null = null;
	let turnStatus: TurnStatus | null = null;
	let error: string | null = null;
	for (const turn of thread.turns) {
		turnId = turn.id;
		turnStatus = turn.status;
		error = turn.error;
		for (const item of turn.items) {
			items.push(item);
		}
	}
	const preserved: readonly string[] = knownChildren[thread.summary.id] ?? [];
	return {
		threadId: thread.summary.id,
		turnId,
		turnStatus,
		items,
		error,
		plan: null,
		childThreadIds: [...preserved],
	};
}

function sameThread(state: TranscriptState, threadId: string): boolean {
	return state.threadId !== null && state.threadId === threadId;
}

function withItems(
	state: TranscriptState,
	items: readonly TranscriptItem[],
): TranscriptState {
	return { ...state, items };
}

function mergeStartedItem(
	existing: TranscriptItem,
	started: TranscriptItem,
): TranscriptItem {
	if (
		((existing.kind === "agentMessage" && started.kind === "agentMessage") ||
			(existing.kind === "reasoning" && started.kind === "reasoning")) &&
		!existing.complete
	) {
		return { ...started, text: started.text + existing.text };
	}
	if (
		existing.kind === "commandExecution" &&
		started.kind === "commandExecution" &&
		existing.status === "inProgress"
	) {
		return { ...started, output: started.output + existing.output };
	}
	return started;
}

function appendItem(
	items: readonly TranscriptItem[],
	item: TranscriptItem,
): TranscriptItem[] {
	const index = items.findIndex((entry) => entry.id === item.id);
	if (index < 0) return [...items, item];
	const next = items.slice();
	const existing = next[index];
	if (existing === undefined) return next;
	next[index] = mergeStartedItem(existing, item);
	return next;
}

function replaceItem(
	items: readonly TranscriptItem[],
	item: TranscriptItem,
): TranscriptItem[] {
	const index = items.findIndex((entry) => entry.id === item.id);
	if (index < 0) {
		return [...items, item];
	}
	const next = items.slice();
	next[index] = item;
	return next;
}

function appendDelta(
	item: TranscriptItem,
	channel: "agent" | "reasoning" | "command",
	delta: string,
): TranscriptItem {
	if (channel === "command" && item.kind === "commandExecution") {
		return { ...item, output: item.output + delta };
	}
	if (channel === "agent" && item.kind === "agentMessage") {
		return { ...item, text: item.text + delta };
	}
	if (channel === "reasoning" && item.kind === "reasoning") {
		return { ...item, text: item.text + delta };
	}
	return item;
}

function placeholder(
	channel: "agent" | "reasoning" | "command",
	itemId: string,
	delta: string,
): TranscriptItem {
	if (channel === "command") {
		return {
			id: itemId,
			kind: "commandExecution",
			command: "",
			output: delta,
			status: "inProgress",
			exitCode: null,
		};
	}
	if (channel === "reasoning") {
		return { id: itemId, kind: "reasoning", text: delta, complete: false };
	}
	return { id: itemId, kind: "agentMessage", text: delta, complete: false };
}

function applyDelta(
	state: TranscriptState,
	notification: Extract<AppNotification, { type: "delta" }>,
): TranscriptState {
	const index = state.items.findIndex(
		(entry) => entry.id === notification.itemId,
	);
	if (index < 0) {
		return withItems(state, [
			...state.items,
			placeholder(
				notification.channel,
				notification.itemId,
				notification.delta,
			),
		]);
	}
	const current = state.items[index];
	if (current === undefined) {
		return state;
	}
	const next = state.items.slice();
	next[index] = appendDelta(current, notification.channel, notification.delta);
	return withItems(state, next);
}

export function reduceNotification(
	state: TranscriptState,
	notification: AppNotification,
): TranscriptState {
	switch (notification.type) {
		case "threadStarted": {
			if (
				state.threadId !== null &&
				notification.thread.summary.id !== state.threadId
			) {
				return state;
			}
			const rebuilt = transcriptForThread(notification.thread);
			if (state.threadId !== null && state.threadId === rebuilt.threadId) {
				return { ...rebuilt, childThreadIds: [...state.childThreadIds] };
			}
			return rebuilt;
		}
		case "threadStatus":
		case "threadName":
		case "advisorNote":
		case "requestResolved":
			return state;
		case "childSpawned": {
			if (!sameThread(state, notification.threadId)) {
				return state;
			}
			if (state.childThreadIds.includes(notification.childId)) {
				return state;
			}
			return {
				...state,
				childThreadIds: [...state.childThreadIds, notification.childId],
			};
		}
		case "plan": {
			if (!sameThread(state, notification.threadId)) {
				return state;
			}
			const { turnId, explanation, steps } = notification;
			return { ...state, plan: { turnId, explanation, steps } };
		}
		case "turnStarted": {
			if (!sameThread(state, notification.threadId)) {
				return state;
			}
			if (
				state.turnId === notification.turn.id &&
				state.turnStatus !== null &&
				state.turnStatus !== "inProgress"
			) {
				return state;
			}
			return {
				...state,
				turnId: notification.turn.id,
				turnStatus: notification.turn.status,
				error: notification.turn.error,
				plan: state.plan?.turnId === notification.turn.id ? state.plan : null,
			};
		}
		case "turnCompleted": {
			if (!sameThread(state, notification.threadId)) {
				return state;
			}
			return {
				...state,
				turnId: notification.turn.id,
				turnStatus: notification.turn.status,
				error: notification.turn.error,
				plan: state.plan?.turnId === notification.turn.id ? state.plan : null,
			};
		}
		case "itemStarted": {
			if (!sameThread(state, notification.threadId)) {
				return state;
			}
			return {
				...withItems(state, appendItem(state.items, notification.item)),
				turnId: notification.turnId,
			};
		}
		case "itemCompleted": {
			if (!sameThread(state, notification.threadId)) {
				return state;
			}
			return {
				...withItems(state, replaceItem(state.items, notification.item)),
				turnId: notification.turnId,
			};
		}
		case "delta": {
			if (!sameThread(state, notification.threadId)) {
				return state;
			}
			return {
				...applyDelta(state, notification),
				turnId: notification.turnId,
			};
		}
		case "error": {
			if (!sameThread(state, notification.threadId)) {
				return state;
			}
			return {
				...state,
				turnId: notification.turnId,
				turnStatus: notification.willRetry ? state.turnStatus : "failed",
				error: notification.message,
			};
		}
		default:
			return state;
	}
}

export interface OpenThreadState {
	readonly transcript: TranscriptState;
	readonly knownChildren: KnownChildThreads;
	readonly activeThreadId: string | null;
}

export interface OpenThreadUpdate {
	readonly transcript: TranscriptState;
	readonly knownChildren: KnownChildThreads;
	readonly sendFollowup: boolean;
}

export function applyOpenThreadNotification(
	state: OpenThreadState,
	notification: AppNotification,
): OpenThreadUpdate {
	const transcript = reduceNotification(state.transcript, notification);
	const knownChildren =
		notification.type === "childSpawned"
			? recordChildThread(
					state.knownChildren,
					notification.threadId,
					notification.childId,
				)
			: state.knownChildren;
	const sendFollowup =
		notification.type === "turnCompleted" &&
		notification.threadId === state.activeThreadId &&
		notification.turn.status === "completed";
	return { transcript, knownChildren, sendFollowup };
}

export function trackApprovalRequest(
	approvals: readonly ApprovalRequest[],
	request: ApprovalRequest,
): readonly ApprovalRequest[] {
	return [...approvals.filter((entry) => entry.id !== request.id), request];
}

export function resolveApprovalRequest(
	approvals: readonly ApprovalRequest[],
	requestId: RequestId,
): readonly ApprovalRequest[] {
	return approvals.filter((entry) => entry.id !== requestId);
}

function isOpenApproval(
	threadId: string | null,
	childThreadIds: readonly string[],
	entryThreadId: string,
): boolean {
	return (
		threadId !== null &&
		(entryThreadId === threadId || childThreadIds.includes(entryThreadId))
	);
}

export function activeApproval(
	approvals: readonly ApprovalRequest[],
	threadId: string | null,
	childThreadIds: readonly string[],
): ApprovalRequest | null {
	return (
		approvals.find((entry) =>
			isOpenApproval(threadId, childThreadIds, entry.threadId),
		) ?? null
	);
}

export function otherThreadApprovalCount(
	approvals: readonly ApprovalRequest[],
	threadId: string | null,
	childThreadIds: readonly string[],
): number {
	return approvals.filter(
		(entry) => !isOpenApproval(threadId, childThreadIds, entry.threadId),
	).length;
}

export function dropSentFollowup(queue: readonly string[]): readonly string[] {
	return queue.slice(1);
}

export interface ScrollSnapshot {
	readonly count: number;
	readonly lastId: string | null;
}

export function shouldAutoScrollForUpdate(
	running: boolean,
	previous: ScrollSnapshot,
	next: ScrollSnapshot,
): boolean {
	if (!running) {
		return true;
	}
	return previous.count !== next.count || previous.lastId !== next.lastId;
}

export function itemsAwaitingApproval(
	approvals: readonly ApprovalRequest[],
	threadId: string | null,
	childThreadIds: readonly string[],
): ReadonlySet<string> {
	return new Set(
		approvals
			.filter((entry) =>
				isOpenApproval(threadId, childThreadIds, entry.threadId),
			)
			.map((entry) => entry.itemId),
	);
}

export function itemDisplayStatus(
	status: string,
	itemId: string,
	awaitingApproval: ReadonlySet<string>,
): string {
	return status === "inProgress" && awaitingApproval.has(itemId)
		? "awaitingApproval"
		: status;
}

export function isSentFollowupAtHead(
	queue: readonly string[],
	sent: string,
): boolean {
	return queue[0] === sent;
}

export function requeueFailedFollowup(
	queue: readonly string[],
	text: string,
): readonly string[] {
	return [...queue, text];
}
