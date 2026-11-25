import {
	isDefaultIgnorable,
	isLineBreak,
	isWhiteSpace,
} from "#lib/text/validate.ts";

function isControl(cp: number): boolean {
	return cp < 0x20 || (cp >= 0x7f && cp <= 0x9f);
}

function isBidiControl(cp: number): boolean {
	return (
		cp === 0x061c ||
		cp === 0x200e ||
		cp === 0x200f ||
		(cp >= 0x202a && cp <= 0x202e) ||
		(cp >= 0x2066 && cp <= 0x2069)
	);
}

export function visible(cp: number): string {
	return `<U+${cp.toString(16).toUpperCase().padStart(4, "0")}>`;
}

function project(text: string, block: boolean): string {
	let out = "";
	const source = text.replace(/\r\n/g, "\n");
	for (const char of source) {
		const cp = char.codePointAt(0) ?? 0;
		if (isLineBreak(cp)) out += block ? "\n" : " ";
		else if (cp === 0x09) out += block ? "    " : " ";
		else if (isControl(cp) || isBidiControl(cp)) out += visible(cp);
		else out += char;
	}
	return out;
}

function trimWhiteSpace(text: string): string {
	const chars = Array.from(text);
	const kept = (char: string): boolean =>
		!isWhiteSpace(char.codePointAt(0) ?? 0);
	const start = chars.findIndex(kept);
	if (start < 0) return "";
	return chars.slice(start, chars.findLastIndex(kept) + 1).join("");
}

export function displayLine(text: string): string {
	return trimWhiteSpace(project(text, false));
}

export function displayBlock(text: string): string {
	const lines = project(text, true).split("\n");
	const first = lines.findIndex((line) => trimWhiteSpace(line) !== "");
	if (first < 0) return "";
	return lines.slice(first).join("\n").replace(/ +$/, "");
}

export function exposed(text: string): string {
	let out = "";
	for (const char of text) {
		const cp = char.codePointAt(0) ?? 0;
		out += cp > 0x20 && cp < 0x7f && char !== "<" ? char : visible(cp);
	}
	return out;
}

function visibleKey(line: string): string {
	let out = "";
	for (const char of line) {
		if (!isDefaultIgnorable(char.codePointAt(0) ?? 0)) out += char;
	}
	return out;
}

export function quote(text: string): string {
	let out = '"';
	for (const char of text) {
		const cp = char.codePointAt(0) ?? 0;
		if (char === '"' || char === "\\") out += `\\${char}`;
		else if (isControl(cp) || isLineBreak(cp) || isBidiControl(cp)) {
			out += `\\u${cp.toString(16).padStart(4, "0")}`;
		} else out += char;
	}
	return `${out}"`;
}

function editable(cp: number): boolean {
	return (
		cp === 0x09 ||
		cp === 0x0a ||
		(!isControl(cp) && !isBidiControl(cp) && !isLineBreak(cp))
	);
}

export function escaped(text: string): string {
	let out = "";
	for (const char of text) {
		const cp = char.codePointAt(0) ?? 0;
		out += editable(cp) ? char : visible(cp);
	}
	return out;
}

export function distinctLines(sources: readonly string[]): string[] {
	const lines = sources.map(displayLine);
	const firstSource = new Map<string, string>();
	const collides = new Set<string>();
	sources.forEach((source, index) => {
		const key = visibleKey(lines[index] ?? "");
		const first = firstSource.get(key);
		if (first === undefined) firstSource.set(key, source);
		else if (first !== source) collides.add(key);
	});
	return sources.map((source, index) => {
		const line = lines[index] ?? "";
		return collides.has(visibleKey(line)) ? exposed(source) : line;
	});
}
