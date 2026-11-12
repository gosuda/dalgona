import { describe, expect, test } from "vitest";
import {
	attachmentsOf,
	type ImageChip,
	messageInput,
	shellText,
} from "./attachments";

const shot: ImageChip = {
	kind: "image",
	id: 2,
	name: "shot.png",
	mime: "image/png",
	size: 8,
	dataUrl: "data:image/png;base64,iVBORw0KGgo=",
};

describe("messageInput", () => {
	test("puts the typed text first, then each paste, then the images", () => {
		expect(
			messageInput("please summarise", {
				texts: ["line 1\nline 2"],
				images: [shot],
			}),
		).toEqual([
			{
				type: "text",
				text: "please summarise\n\nline 1\nline 2",
				text_elements: [],
			},
			{ type: "image", url: "data:image/png;base64,iVBORw0KGgo=" },
		]);
	});

	test("omits an empty typed text when attachments carry the message", () => {
		expect(messageInput("", { texts: ["pasted"], images: [] })).toEqual([
			{ type: "text", text: "pasted", text_elements: [] },
		]);
	});
});

describe("attachmentsOf", () => {
	test("sends a shell chip's text as one more text part", () => {
		expect(
			attachmentsOf([
				{
					kind: "shell",
					id: 3,
					n: 1,
					exitCode: 0,
					lines: 1,
					text: "Output of `ls` (exit 0):",
				},
			]),
		).toEqual({ texts: ["Output of `ls` (exit 0):"], images: [] });
	});
});

describe("shellText", () => {
	test("fences the output with a fence longer than any backtick run in it", () => {
		expect(shellText("cat notes.md", 0, "a\n```\nb")).toBe(
			"Output of `cat notes.md` (exit 0):\n````\na\n```\nb\n````",
		);
	});

	test("keeps the three-backtick fence when the output has no longer run", () => {
		expect(shellText("ls", 0, "x `` y")).toBe(
			"Output of `ls` (exit 0):\n```\nx `` y\n```",
		);
	});
});
