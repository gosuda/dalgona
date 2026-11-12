import { describe, expect, test } from "vitest";
import type { TranscriptItem } from "#lib/types.ts";
import {
	groupTools,
	lineSummary,
	parseVerbosity,
	presentRow,
	toolGroupSummary,
} from "#lib/verbosity.ts";

const evalCall: TranscriptItem = {
	id: "call-1",
	kind: "toolCall",
	name: "eval",
	arguments: {
		code: "tools.todo(items = [])\ntools.web_fetch(url = 'https://example.com')",
	},
	output: "",
	status: "completed",
	durationMs: null,
};
const todoCall: TranscriptItem = {
	id: "run-1/1",
	kind: "toolCall",
	name: "todo",
	arguments: { items: [] },
	output: "",
	status: "completed",
	durationMs: null,
};
const ls: TranscriptItem = {
	id: "run-1/2",
	kind: "commandExecution",
	command: "ls",
	output: "a.txt\nb.txt",
	status: "completed",
	exitCode: 0,
	durationMs: null,
};
const pwd: TranscriptItem = {
	id: "run-1/3",
	kind: "commandExecution",
	command: "pwd",
	output: "/repo",
	status: "completed",
	exitCode: 0,
	durationMs: null,
};
const webFetch: TranscriptItem = {
	id: "run-1/4",
	kind: "toolCall",
	name: "web_fetch",
	arguments: { url: "https://example.com" },
	output: "ok",
	status: "completed",
	durationMs: null,
};
const fixture = [evalCall, todoCall, ls, pwd, webFetch];

describe("verbosity", () => {
	test("reads the setting and defaults to medium", () => {
		expect(parseVerbosity("high")).toBe("high");
		expect(parseVerbosity("low")).toBe("low");
		expect(parseVerbosity("loud")).toBe("medium");
		expect(parseVerbosity(undefined)).toBe("medium");
	});

	test("high shows every row in full", () => {
		for (const item of fixture) {
			expect(presentRow(item, "high", false)).toEqual({
				mode: "full",
				tailLines: 0,
			});
		}
	});

	test("medium shows a reasoning row as one line, and a streaming one as its tail", () => {
		const reasoning: TranscriptItem = {
			id: "r1",
			kind: "reasoning",
			text: "a\nb\nc",
			complete: true,
		};
		expect(presentRow(reasoning, "medium", false).mode).toBe("line");
		expect(
			presentRow({ ...reasoning, complete: false }, "medium", true),
		).toEqual({
			mode: "tail",
			tailLines: 1,
		});
	});

	test("medium keeps the last 10 command lines and 60 diff lines per file", () => {
		expect(presentRow(ls, "medium", false)).toEqual({
			mode: "tail",
			tailLines: 10,
		});
		const diff: TranscriptItem = {
			id: "f1",
			kind: "fileChange",
			changes: [],
			status: "completed",
			review: null,
		};
		expect(presentRow(diff, "medium", false)).toEqual({
			mode: "tail",
			tailLines: 60,
		});
	});

	test("a failed tool row keeps its last 10 lines at every level", () => {
		const failed: TranscriptItem = { ...webFetch, status: "failed" };
		expect(presentRow(failed, "low", false)).toEqual({
			mode: "tail",
			tailLines: 10,
		});
		expect(presentRow(failed, "medium", false)).toEqual({
			mode: "tail",
			tailLines: 10,
		});
	});

	test("a failed row keeps its last 10 lines at high too", () => {
		const failed: TranscriptItem = { ...ls, status: "failed" };
		expect(presentRow(failed, "high", false)).toEqual({
			mode: "tail",
			tailLines: 10,
		});
	});

	test("low hides tool rows and groups consecutive tool calls", () => {
		for (const item of fixture) {
			expect(presentRow(item, "low", false).mode).toBe("hidden");
		}
		const entries = groupTools(fixture, "low");
		expect(entries).toHaveLength(1);
		const [group] = entries;
		expect(group && "items" in group ? group.items : []).toHaveLength(5);
		expect(group && "items" in group ? toolGroupSummary(group) : "").toBe(
			"Ran 2 commands · called 3 tools",
		);
	});

	test("low ends a group at a failed tool row", () => {
		const failedLs: TranscriptItem = {
			...ls,
			id: "run-1/9",
			status: "failed",
			exitCode: 1,
		};
		const entries = groupTools([ls, failedLs, pwd], "low");
		expect(entries).toHaveLength(3);
		const [before, failedRow, after] = entries;
		expect(before && "items" in before ? before.items : []).toEqual([ls]);
		expect(failedRow).toEqual(failedLs);
		expect(after && "items" in after ? after.items : []).toEqual([pwd]);
	});

	test("the medium summaries name the eval code and the reasoning lines", () => {
		expect(lineSummary(evalCall)).toBe("Eval · 2 lines of code · Done");
		expect(
			lineSummary({
				id: "r1",
				kind: "reasoning",
				text: "a\nb\nc",
				complete: true,
			}),
		).toBe("Thinking · 3 lines · ctrl+o");
	});

	test("medium and high leave rows ungrouped", () => {
		expect(groupTools(fixture, "medium")).toEqual(fixture);
		expect(groupTools(fixture, "high")).toEqual(fixture);
	});

	test("shows an eval call and its nested calls at each level", () => {
		expect(fixture.map((item) => presentRow(item, "high", false).mode)).toEqual(
			["full", "full", "full", "full", "full"],
		);
		expect(fixture.map((item) => presentRow(item, "medium", false))).toEqual([
			{ mode: "line", tailLines: 0 },
			{ mode: "full", tailLines: 0 },
			{ mode: "tail", tailLines: 10 },
			{ mode: "tail", tailLines: 10 },
			{ mode: "full", tailLines: 0 },
		]);
		expect(fixture.map((item) => presentRow(item, "low", false).mode)).toEqual([
			"hidden",
			"hidden",
			"hidden",
			"hidden",
			"hidden",
		]);
	});
});
