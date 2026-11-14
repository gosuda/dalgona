import { themeSettings } from "./theme/theme";
import type { JsonValue } from "./types";
import type { SettingValue } from "./validation";

interface ChoiceOption {
	readonly value: string;
	readonly label: string;
}

export interface SettingsDraft {
	readonly approval: string;
	readonly verbosity: string;
	readonly theme: string;
	readonly statusHide: readonly string[];
	readonly editReview: boolean;
}

export interface SettingEdit {
	readonly keyPath: string;
	readonly value: JsonValue;
}

export const approvalOptions: readonly ChoiceOption[] = [
	{ value: "ask", label: "Ask" },
	{ value: "auto", label: "Auto" },
	{ value: "yolo", label: "Yolo" },
];

export const verbosityOptions: readonly ChoiceOption[] = [
	{ value: "low", label: "Low" },
	{ value: "medium", label: "Medium" },
	{ value: "high", label: "High" },
];

export const themeOptions: readonly ChoiceOption[] = [
	{ value: "auto", label: "Auto" },
	{ value: "auto-soft", label: "Auto soft" },
	{ value: "day", label: "Day" },
	{ value: "soft-day", label: "Soft day" },
	{ value: "night", label: "Night" },
	{ value: "soft-night", label: "Soft night" },
];

export const hideableItemOptions: readonly ChoiceOption[] = [
	{ value: "effort", label: "Effort" },
	{ value: "approval", label: "Approval" },
	{ value: "sandbox", label: "Sandbox" },
	{ value: "folder", label: "Folder" },
	{ value: "queue", label: "Queue" },
	{ value: "context", label: "Context" },
	{ value: "mark", label: "Product mark" },
];

const enumChoices: readonly {
	readonly key: "approval" | "verbosity" | "theme";
	readonly label: string;
	readonly values: readonly string[];
}[] = [
	{
		key: "approval",
		label: "policy for approvals",
		values: approvalOptions.map((option) => option.value),
	},
	{
		key: "verbosity",
		label: "level of detail",
		values: verbosityOptions.map((option) => option.value),
	},
	{ key: "theme", label: "theme", values: themeSettings },
];

function textSetting(values: readonly SettingValue[], key: string): string {
	const value = values.find((entry) => entry.key === key)?.value;
	return typeof value === "string" ? value : "";
}

function listSetting(
	values: readonly SettingValue[],
	key: string,
): readonly string[] {
	const value = values.find((entry) => entry.key === key)?.value;
	if (!Array.isArray(value)) return [];
	return value.filter((item): item is string => typeof item === "string");
}

export function draftFromSettings(
	values: readonly SettingValue[],
): SettingsDraft {
	return {
		approval: textSetting(values, "approval"),
		verbosity: textSetting(values, "verbosity"),
		theme: textSetting(values, "theme"),
		statusHide: listSetting(values, "statusHide"),
		editReview:
			values.find((entry) => entry.key === "editReview")?.value === true,
	};
}

export function draftProblem(draft: SettingsDraft): string | null {
	for (const choice of enumChoices) {
		if (!choice.values.includes(draft[choice.key])) {
			return `Choose a ${choice.label}.`;
		}
	}
	return null;
}

function sortedItems(items: readonly string[]): string {
	return [...items].sort((a, b) => a.localeCompare(b)).join("\n");
}

export function settingEdits(
	saved: SettingsDraft,
	draft: SettingsDraft,
): readonly SettingEdit[] {
	const edits: SettingEdit[] = [];
	if (draft.approval !== saved.approval) {
		edits.push({ keyPath: "approval", value: draft.approval });
	}
	if (draft.verbosity !== saved.verbosity) {
		edits.push({ keyPath: "verbosity", value: draft.verbosity });
	}
	if (draft.theme !== saved.theme) {
		edits.push({ keyPath: "theme", value: draft.theme });
	}
	if (sortedItems(saved.statusHide) !== sortedItems(draft.statusHide)) {
		edits.push({ keyPath: "statusHide", value: [...draft.statusHide] });
	}
	if (draft.editReview !== saved.editReview) {
		edits.push({ keyPath: "editReview", value: draft.editReview });
	}
	return edits;
}
