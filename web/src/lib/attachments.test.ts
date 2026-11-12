import { describe, expect, test } from "vitest";
import { type ImageChip, messageInput } from "./attachments";

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
