import type { KeywordDefinition } from "./types.js";

export interface KeywordMatch {
	readonly word: string;
	readonly start: number;
	readonly end: number;
	readonly hue: readonly [number, number];
}

const LETTER = /^\p{L}$/u;
const DIGIT = /^\p{Nd}$/u;

interface ArmedWord {
	readonly word: string;
	readonly glyphs: readonly string[];
	readonly hue: readonly [number, number];
}

interface FenceMark {
	readonly mark: "`" | "~";
	readonly count: number;
}

export function findKeywords(
	prompt: string,
	table: readonly KeywordDefinition[],
): readonly KeywordMatch[] {
	const points = Array.from(prompt);
	const masked = maskProse(points);
	const words = arm(table);
	const matches: KeywordMatch[] = [];
	let index = 0;
	while (index < points.length) {
		const hit = matchAt(points, masked, words, index);
		if (hit === undefined) {
			index += 1;
			continue;
		}
		matches.push(hit);
		index = hit.end;
	}
	return matches;
}

export function keywordGradient(hue: readonly [number, number]): string {
	const stops: string[] = [];
	for (let step = 0; step < 14; step += 1) {
		const t = step / 13;
		const degrees = hue[0] + (hue[1] - hue[0]) * t;
		stops.push(
			`oklch(0.82 0.13 ${degrees.toFixed(2)}) ${(t * 100).toFixed(2)}%`,
		);
	}
	return `linear-gradient(90deg, ${stops.join(", ")})`;
}

function arm(table: readonly KeywordDefinition[]): readonly ArmedWord[] {
	const seen = new Set<string>();
	const words: ArmedWord[] = [];
	for (const entry of table) {
		if (entry.word.length === 0 || seen.has(entry.word)) {
			continue;
		}
		seen.add(entry.word);
		words.push({
			word: entry.word,
			glyphs: Array.from(entry.word),
			hue: entry.hue,
		});
	}
	return words.toSorted(
		(left, right) => right.glyphs.length - left.glyphs.length,
	);
}

function matchAt(
	points: readonly string[],
	masked: readonly boolean[],
	words: readonly ArmedWord[],
	index: number,
): KeywordMatch | undefined {
	if (masked[index] === true) {
		return undefined;
	}
	for (const entry of words) {
		const end = index + entry.glyphs.length;
		if (!occupies(points, masked, entry.glyphs, index)) {
			continue;
		}
		if (!beforeOk(points, index) || !afterOk(points, end)) {
			continue;
		}
		return { word: entry.word, start: index, end, hue: entry.hue };
	}
	return undefined;
}

function occupies(
	points: readonly string[],
	masked: readonly boolean[],
	glyphs: readonly string[],
	index: number,
): boolean {
	if (index + glyphs.length > points.length) {
		return false;
	}
	for (let offset = 0; offset < glyphs.length; offset += 1) {
		if (
			points[index + offset] !== glyphs[offset] ||
			masked[index + offset] === true
		) {
			return false;
		}
	}
	return true;
}

function beforeOk(points: readonly string[], index: number): boolean {
	const previous = points[index - 1];
	if (previous === undefined) {
		return true;
	}
	if (
		isIdentEdge(previous) ||
		previous === "." ||
		previous === "/" ||
		previous === "\\"
	) {
		return false;
	}
	const earlier = points[index - 2];
	return !(earlier === ":" && previous === ":");
}

function afterOk(points: readonly string[], end: number): boolean {
	const next = points[end];
	if (next === undefined) {
		return true;
	}
	if (isIdentEdge(next) || next === "/" || next === "\\" || next === "(") {
		return false;
	}
	if (next !== ".") {
		return true;
	}
	const afterDot = points[end + 1];
	if (afterDot === undefined) {
		return true;
	}
	return !(
		isLetter(afterDot) ||
		isDigit(afterDot) ||
		afterDot === "_" ||
		afterDot === "-"
	);
}

function isIdentEdge(glyph: string): boolean {
	return isLetter(glyph) || isDigit(glyph) || glyph === "_" || glyph === "-";
}

function isLetter(glyph: string): boolean {
	return LETTER.test(glyph);
}

function isDigit(glyph: string): boolean {
	return DIGIT.test(glyph);
}

function maskProse(points: readonly string[]): boolean[] {
	const masked = points.map(() => false);
	maskFences(points, masked);
	maskInlineCode(points, masked);
	maskComments(points, masked);
	maskTags(points, masked);
	return masked;
}

function maskFences(points: readonly string[], masked: boolean[]): void {
	let index = 0;
	while (index < points.length) {
		const lineEnd = lineEndAt(points, index);
		const opener = openingFence(points, index, lineEnd);
		if (opener === undefined || spanMasked(masked, index, lineEnd)) {
			index = lineEnd + 1;
			continue;
		}
		const close = closingFenceEnd(points, lineEnd + 1, opener);
		const end = close ?? points.length;
		fill(masked, index, end);
		index = end;
	}
}

function closingFenceEnd(
	points: readonly string[],
	from: number,
	opener: FenceMark,
): number | undefined {
	let index = from;
	while (index < points.length) {
		const lineEnd = lineEndAt(points, index);
		const closer = closingFence(points, index, lineEnd);
		if (
			closer !== undefined &&
			closer.mark === opener.mark &&
			closer.count >= opener.count
		) {
			return lineEnd + 1;
		}
		index = lineEnd + 1;
	}
	return undefined;
}

function openingFence(
	points: readonly string[],
	start: number,
	lineEnd: number,
): FenceMark | undefined {
	const mark = fenceRun(points, start, lineEnd);
	if (mark === undefined) {
		return undefined;
	}
	if (mark.mark === "`" && points.slice(mark.end, lineEnd).includes("`")) {
		return undefined;
	}
	return mark;
}

function closingFence(
	points: readonly string[],
	start: number,
	lineEnd: number,
): FenceMark | undefined {
	const mark = fenceRun(points, start, lineEnd);
	if (mark === undefined) {
		return undefined;
	}
	const rest = points.slice(mark.end, lineEnd);
	if (rest.some((glyph) => glyph !== " " && glyph !== "\t" && glyph !== "\r")) {
		return undefined;
	}
	return mark;
}

function fenceRun(
	points: readonly string[],
	start: number,
	lineEnd: number,
): (FenceMark & { readonly end: number }) | undefined {
	let index = start;
	let indent = 0;
	while (index < lineEnd && points[index] === " " && indent < 3) {
		indent += 1;
		index += 1;
	}
	const mark = points[index];
	if (mark !== "`" && mark !== "~") {
		return undefined;
	}
	let count = 0;
	while (index < lineEnd && points[index] === mark) {
		count += 1;
		index += 1;
	}
	if (count < 3) {
		return undefined;
	}
	return { mark, count, end: index };
}

function maskInlineCode(points: readonly string[], masked: boolean[]): void {
	let index = 0;
	while (index < points.length) {
		if (masked[index] === true || points[index] !== "`") {
			index += 1;
			continue;
		}
		const count = runLength(points, masked, index, "`");
		const close = findRun(points, masked, index + count, "`", count);
		if (close === undefined) {
			index += 1;
			continue;
		}
		fill(masked, index, close + count);
		index = close + count;
	}
}

function maskComments(points: readonly string[], masked: boolean[]): void {
	let index = 0;
	while (index < points.length) {
		if (masked[index] === true || !startsWith(points, index, "<!--")) {
			index += 1;
			continue;
		}
		const close = findSequence(points, masked, index + 4, "-->");
		const end = close === undefined ? points.length : close + 3;
		fill(masked, index, end);
		index = end;
	}
}

function maskTags(points: readonly string[], masked: boolean[]): void {
	let index = 0;
	while (index < points.length) {
		if (masked[index] === true || points[index] !== "<") {
			index += 1;
			continue;
		}
		const end = tagEnd(points, masked, index);
		if (end === undefined) {
			index += 1;
			continue;
		}
		fill(masked, index, end);
		index = end;
	}
}

function tagEnd(
	points: readonly string[],
	masked: readonly boolean[],
	start: number,
): number | undefined {
	let index = start + 1;
	if (points[index] === "/") {
		index += 1;
	}
	const name = points[index];
	if (name === undefined || !(isLetter(name) || name === "_" || name === ":")) {
		return undefined;
	}
	index += 1;
	let quote: '"' | "'" | undefined;
	while (index < points.length) {
		if (masked[index] === true) {
			return undefined;
		}
		const glyph = points[index] ?? "";
		if (quote !== undefined) {
			if (glyph === quote) {
				quote = undefined;
			}
			index += 1;
			continue;
		}
		if (glyph === '"' || glyph === "'") {
			quote = glyph;
			index += 1;
			continue;
		}
		if (glyph === ">") {
			return index + 1;
		}
		index += 1;
	}
	return undefined;
}

function lineEndAt(points: readonly string[], start: number): number {
	let index = start;
	while (index < points.length && points[index] !== "\n") {
		index += 1;
	}
	return index;
}

function runLength(
	points: readonly string[],
	masked: readonly boolean[],
	start: number,
	mark: string,
): number {
	let count = 0;
	while (
		start + count < points.length &&
		masked[start + count] !== true &&
		points[start + count] === mark
	) {
		count += 1;
	}
	return count;
}

function findRun(
	points: readonly string[],
	masked: readonly boolean[],
	from: number,
	mark: string,
	count: number,
): number | undefined {
	for (let index = from; index < points.length; index += 1) {
		if (masked[index] === true) {
			return undefined;
		}
		if (
			points[index] === mark &&
			runLength(points, masked, index, mark) === count
		) {
			return index;
		}
	}
	return undefined;
}

function findSequence(
	points: readonly string[],
	masked: readonly boolean[],
	from: number,
	sequence: string,
): number | undefined {
	const glyphs = Array.from(sequence);
	for (let index = from; index + glyphs.length <= points.length; index += 1) {
		if (masked[index] === true) {
			return undefined;
		}
		if (startsWith(points, index, sequence)) {
			return index;
		}
	}
	return undefined;
}

function startsWith(
	points: readonly string[],
	index: number,
	sequence: string,
): boolean {
	const glyphs = Array.from(sequence);
	if (index + glyphs.length > points.length) {
		return false;
	}
	for (let offset = 0; offset < glyphs.length; offset += 1) {
		if (points[index + offset] !== glyphs[offset]) {
			return false;
		}
	}
	return true;
}

function spanMasked(
	masked: readonly boolean[],
	start: number,
	end: number,
): boolean {
	for (let index = start; index < end; index += 1) {
		if (masked[index] === true) {
			return true;
		}
	}
	return false;
}

function fill(masked: boolean[], start: number, end: number): void {
	for (let index = start; index < end && index < masked.length; index += 1) {
		masked[index] = true;
	}
}
