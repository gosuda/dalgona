import { countLabel, itemStatusLabel } from "#lib/labels.ts";
import type { JsonValue, TranscriptItem } from "#lib/types.ts";

export const verbosityLevels = ["low", "medium", "high"] as const;

export type Verbosity = (typeof verbosityLevels)[number];

export interface RowPresentation {
	readonly mode: "full" | "line" | "tail" | "hidden";
	readonly tailLines: number;
}

export interface ToolGroup {
	readonly items: readonly TranscriptItem[];
}

export type TranscriptEntry = TranscriptItem | ToolGroup;

type ToolItem = Extract<
	TranscriptItem,
	{ readonly kind: "commandExecution" | "fileChange" | "toolCall" }
>;

export const fullPresentation: RowPresentation = { mode: "full", tailLines: 0 };
const linePresentation: RowPresentation = { mode: "line", tailLines: 0 };
const hiddenPresentation: RowPresentation = { mode: "hidden", tailLines: 0 };
const commandTailLines = 10;
const diffLines = 60;

export function parseVerbosity(value: unknown): Verbosity {
	return verbosityLevels.find((level) => level === value) ?? "medium";
}

function tailOf(tailLines: number): RowPresentation {
	return { mode: "tail", tailLines };
}

function isToolItem(item: TranscriptItem): item is ToolItem {
	return (
		item.kind === "commandExecution" ||
		item.kind === "fileChange" ||
		item.kind === "toolCall"
	);
}

function failed(item: TranscriptItem): boolean {
	return isToolItem(item) && item.status === "failed";
}

function mediumToolRow(item: ToolItem): RowPresentation {
	switch (item.kind) {
		case "commandExecution":
			return tailOf(commandTailLines);
		case "fileChange":
			return tailOf(diffLines);
		case "toolCall":
			return item.name === "eval" ? linePresentation : fullPresentation;
	}
}

export function presentRow(
	item: TranscriptItem,
	level: Verbosity,
	live: boolean,
): RowPresentation {
	if (isToolItem(item) && failed(item)) return tailOf(commandTailLines);
	if (level === "high") return fullPresentation;
	if (item.kind === "reasoning") {
		return live ? tailOf(1) : linePresentation;
	}
	if (!isToolItem(item)) return fullPresentation;
	if (level === "low") return hiddenPresentation;
	return mediumToolRow(item);
}

export function isLiveItem(item: TranscriptItem): boolean {
	switch (item.kind) {
		case "agentMessage":
		case "reasoning":
			return !item.complete;
		case "commandExecution":
		case "fileChange":
		case "toolCall":
			return item.status === "inProgress";
		default:
			return false;
	}
}

export function isFinishedEntry(entry: TranscriptEntry): boolean {
	if (isToolGroup(entry)) return entry.items.every((item) => !isLiveItem(item));
	return !isLiveItem(entry);
}

export function isToolGroup(entry: TranscriptEntry): entry is ToolGroup {
	return "items" in entry;
}

export function groupTools(
	items: readonly TranscriptItem[],
	level: Verbosity,
): readonly TranscriptEntry[] {
	if (level !== "low") return items;
	const entries: TranscriptEntry[] = [];
	let group: TranscriptItem[] = [];
	const flush = (): void => {
		if (group.length > 0) entries.push({ items: group });
		group = [];
	};
	for (const item of items) {
		if (isToolItem(item) && !failed(item)) {
			group.push(item);
			continue;
		}
		flush();
		entries.push(item);
	}
	flush();
	return entries;
}

export function toolGroupSummary(group: ToolGroup): string {
	let commands = 0;
	let edits = 0;
	let calls = 0;
	for (const item of group.items) {
		if (item.kind === "commandExecution") commands += 1;
		else if (item.kind === "fileChange")
			edits += Math.max(item.changes.length, 1);
		else if (item.kind === "toolCall") calls += 1;
	}
	const parts: string[] = [];
	if (commands > 0)
		parts.push(`Ran ${countLabel(commands, "command", "commands")}`);
	if (edits > 0) parts.push(`edited ${countLabel(edits, "file", "files")}`);
	if (calls > 0) parts.push(`called ${countLabel(calls, "tool", "tools")}`);
	return parts.join(" · ");
}

function isRecord(
	value: JsonValue,
): value is { readonly [key: string]: JsonValue } {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function evalCode(args: JsonValue): string {
	if (!isRecord(args)) return "";
	const { code } = args;
	return typeof code === "string" ? code : "";
}

function lineCount(text: string): number {
	const trimmed = text.replace(/\n+$/u, "");
	return trimmed === "" ? 0 : trimmed.split("\n").length;
}

export function lineSummary(item: TranscriptItem): string {
	if (item.kind === "reasoning") {
		return `Thinking · ${countLabel(lineCount(item.text), "line", "lines")} · ctrl+o`;
	}
	if (item.kind === "toolCall") {
		const code = evalCode(item.arguments);
		const lines = countLabel(lineCount(code), "line", "lines");
		return `Eval · ${lines} of code · ${itemStatusLabel(item.status)}`;
	}
	return "";
}

export function lastLines(
	text: string,
	count: number,
): { readonly text: string; readonly total: number } {
	const lines = text.replace(/\n$/u, "").split("\n");
	return { text: lines.slice(-count).join("\n"), total: lines.length };
}
