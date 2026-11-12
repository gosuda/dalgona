import { describe, expect, test } from "vitest";
import { parseComposerText, slashCommands } from "#lib/commands.ts";

describe("composer text", () => {
	test("// sends a literal slash", () => {
		expect(parseComposerText("//etc is odd")).toEqual({
			kind: "message",
			text: "/etc is odd",
		});
	});

	test("/mod is a command query", () => {
		expect(parseComposerText("/mod")).toEqual({
			kind: "command",
			query: "mod",
		});
	});

	test("/model foo is a command query for the whole text, so no command matches", () => {
		expect(parseComposerText("/model foo")).toEqual({
			kind: "command",
			query: "model foo",
		});
	});

	test("/etc/hosts with a space is a message", () => {
		expect(parseComposerText("/etc/hosts with a space")).toEqual({
			kind: "message",
			text: "/etc/hosts with a space",
		});
	});

	test("plain text is a message", () => {
		expect(parseComposerText("fix the test")).toEqual({
			kind: "message",
			text: "fix the test",
		});
	});

	test("a leading ! is a shell line", () => {
		expect(parseComposerText("!  ls -la ")).toEqual({
			kind: "shell",
			command: "ls -la",
		});
	});
});

describe("slash commands", () => {
	test("lists the twelve commands in order, each with its panel", () => {
		expect(
			slashCommands.map((command) => [command.name, command.panel]),
		).toEqual([
			["model", "model"],
			["settings", "settings"],
			["login", "login"],
			["resume", "resume"],
			["new", "new"],
			["compact", "compact"],
			["fork", "fork"],
			["tree", "tree"],
			["revert", "rewind"],
			["advisor", "advisor"],
			["review", "review"],
			["help", "keys"],
		]);
	});
});
