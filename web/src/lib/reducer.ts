import type {
	AppNotification,
	ThreadDetail,
	TranscriptItem,
	TranscriptState,
	TurnStatus,
} from "./types";

export function emptyTranscript(threadId: string | null): TranscriptState {
	return {
		threadId,
		turnId: null,
		turnStatus: null,
		items: [],
		error: null,
		plan: null,
	};
}

export function transcriptForThread(thread: ThreadDetail): TranscriptState {
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
	return {
		threadId: thread.summary.id,
		turnId,
		turnStatus,
		items,
		error,
		plan: null,
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
		case "threadStarted":
			if (
				state.threadId !== null &&
				notification.thread.summary.id !== state.threadId
			) {
				return state;
			}
			return transcriptForThread(notification.thread);
		case "threadStatus":
		case "threadName":
		case "advisorNote":
		case "requestResolved":
			return state;
		case "plan": {
			if (!sameThread(state, notification.threadId)) {
				return state;
			}
			const { turnId, explanation, steps } = notification;
			return { ...state, plan: { turnId, explanation, steps } };
		}
		case "turnStarted":
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
