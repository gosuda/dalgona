import type {
	AdvisorStatus,
	AdvisorSummary,
	PlanStepStatus,
	ThreadSummary,
} from "./types";

const advisorStatusLabels: Record<AdvisorStatus, string> = {
	idle: "idle",
	reviewing: "reviewing",
	halted: "halted",
	disabled: "off",
};

const plural = new Intl.PluralRules("en");

export function countLabel(n: number, one: string, many: string): string {
	return `${n} ${plural.select(n) === "one" ? one : many}`;
}

export function advisorLabel(summary: AdvisorSummary): string | null {
	switch (summary.kind) {
		case "unknown":
			return null;
		case "off":
			return "off";
		case "unavailable":
			return "unavailable";
		case "loaded": {
			const [only, ...rest] = summary.advisors;
			if (only === undefined) return "none";
			if (rest.length === 0) {
				return `${advisorStatusLabels[only.status]} · ${countLabel(only.notes, "note", "notes")}`;
			}
			const reviewing = summary.advisors.filter(
				(entry) => entry.status === "reviewing",
			).length;
			return `${summary.advisors.length} advisors · ${reviewing} reviewing`;
		}
	}
}

export function threadTitle(
	thread: Pick<ThreadSummary, "title" | "preview">,
): string {
	return thread.title || thread.preview || "Untitled thread";
}

export type StateKey =
	| "inProgress"
	| "awaitingApproval"
	| "completed"
	| "failed"
	| "declined"
	| "interrupted"
	| "idle"
	| "disconnected";

export const stateWords: Readonly<Record<StateKey, string>> = {
	inProgress: "Working",
	awaitingApproval: "Waiting",
	completed: "Done",
	failed: "Failed",
	declined: "Declined",
	interrupted: "Stopped",
	idle: "Ready",
	disconnected: "Offline",
};

const stateKeys: Readonly<Record<string, StateKey>> = {
	active: "inProgress",
	idle: "idle",
	notLoaded: "idle",
	systemError: "failed",
	inProgress: "inProgress",
	awaitingApproval: "awaitingApproval",
	completed: "completed",
	failed: "failed",
	declined: "declined",
	interrupted: "interrupted",
	disconnected: "disconnected",
};

const waitingFlags: readonly string[] = [
	"waitingOnApproval",
	"waitingOnUserInput",
];

export function stateKey(
	status: string,
	activeFlags: readonly string[] = [],
): StateKey | null {
	if (
		status === "active" &&
		activeFlags.some((flag) => waitingFlags.includes(flag))
	) {
		return "awaitingApproval";
	}
	return Object.hasOwn(stateKeys, status) ? (stateKeys[status] ?? null) : null;
}

export function itemStatusLabel(status: string): string {
	const key = stateKey(status);
	return key === null ? status : stateWords[key];
}

const planStatusLabels: Record<PlanStepStatus, string> = {
	pending: "Pending",
	inProgress: "Working",
	completed: "Done",
};

export function planStatusLabel(status: PlanStepStatus): string {
	return planStatusLabels[status];
}

export function kindLabel(kind: string): string {
	const words = kind.replace(/([a-z0-9])([A-Z])/g, "$1 $2").toLowerCase();
	return words.charAt(0).toUpperCase() + words.slice(1);
}
