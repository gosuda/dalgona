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

function noteCount(count: number): string {
	return `${count} ${plural.select(count) === "one" ? "note" : "notes"}`;
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
				return `${advisorStatusLabels[only.status]} · ${noteCount(only.notes)}`;
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

const itemStatusLabels: Readonly<Record<string, string>> = {
	inProgress: "Running",
	completed: "Done",
	failed: "Failed",
	declined: "Declined",
};

export function itemStatusLabel(status: string): string {
	return itemStatusLabels[status] ?? status;
}

const planStatusLabels: Record<PlanStepStatus, string> = {
	pending: "Pending",
	inProgress: "In progress",
	completed: "Done",
};

export function planStatusLabel(status: PlanStepStatus): string {
	return planStatusLabels[status];
}

export function kindLabel(kind: string): string {
	const words = kind.replace(/([a-z0-9])([A-Z])/g, "$1 $2").toLowerCase();
	return words.charAt(0).toUpperCase() + words.slice(1);
}
