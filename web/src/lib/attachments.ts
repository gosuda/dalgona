import type { JsonValue } from "./types";

export const PASTE_CHIP_LINES: number = 10;
export const MAX_IMAGES: number = 10;
export const MAX_IMAGE_BYTES: number = 5 * 1024 * 1024;
export const MAX_MESSAGE_BYTES: number = 15 * 1024 * 1024;
export const IMAGE_TYPES: readonly string[] = [
	"image/png",
	"image/jpeg",
	"image/gif",
];

interface PasteChip {
	readonly kind: "paste";
	readonly id: number;
	readonly text: string;
	readonly lines: number;
}

export interface ImageChip {
	readonly kind: "image";
	readonly id: number;
	readonly name: string;
	readonly mime: string;
	readonly size: number;
	readonly dataUrl: string;
}

export interface ShellRun {
	readonly n: number;
	readonly exitCode: number;
	readonly lines: number;
	readonly text: string;
}

interface ShellChip extends ShellRun {
	readonly kind: "shell";
	readonly id: number;
}

export type Chip = PasteChip | ImageChip | ShellChip;

export interface Attachments {
	readonly texts: readonly string[];
	readonly images: readonly ImageChip[];
}

export const NO_ATTACHMENTS: Attachments = { texts: [], images: [] };

export function attachmentsOf(chips: readonly Chip[]): Attachments {
	const texts: string[] = [];
	const images: ImageChip[] = [];
	for (const chip of chips) {
		if (chip.kind === "paste" || chip.kind === "shell") texts.push(chip.text);
		else images.push(chip);
	}
	return { texts, images };
}

export function messageInput(
	typed: string,
	attachments: Attachments,
): readonly JsonValue[] {
	const parts = [typed, ...attachments.texts].filter((part) => part !== "");
	const items: JsonValue[] = [];
	if (parts.length > 0) {
		items.push({ type: "text", text: parts.join("\n\n"), text_elements: [] });
	}
	for (const image of attachments.images) {
		items.push({ type: "image", url: image.dataUrl });
	}
	return items;
}

export function messageBytes(typed: string, attachments: Attachments): number {
	return new Blob([JSON.stringify(messageInput(typed, attachments))]).size;
}

export function readDataUrl(file: Blob): Promise<string> {
	const { promise, resolve, reject } = Promise.withResolvers<string>();
	const reader = new FileReader();
	reader.addEventListener("load", () => resolve(String(reader.result)));
	reader.addEventListener("error", () =>
		reject(reader.error ?? new Error("cannot read the file")),
	);
	reader.readAsDataURL(file);
	return promise;
}

export function shellOutput(
	stdout: string,
	stderr: string,
): { readonly output: string; readonly lines: number } {
	const output = [stdout, stderr]
		.map((part) => part.replace(/\n+$/, ""))
		.filter((part) => part !== "")
		.join("\n");
	return { output, lines: output === "" ? 0 : output.split("\n").length };
}

export function shellText(
	command: string,
	exitCode: number,
	output: string,
): string {
	const longest = Array.from(output.matchAll(/`+/g)).reduce(
		(most, run) => Math.max(most, run[0].length),
		0,
	);
	const fence = "`".repeat(Math.max(3, longest + 1));
	return `Output of \`${command}\` (exit ${exitCode}):\n${fence}\n${output}\n${fence}`;
}
