import { isImeKey } from "../ime";
import { paletteBits, scalarPalette, scalarRuns } from "./tables";

export interface TextRange {
	readonly start: number;
	readonly end: number;
}

type TextField = HTMLInputElement | HTMLTextAreaElement;

const CLASS_MASK = 0x0f;
const CR = 1;
const LF = 2;
const CONTROL = 3;
const EXTEND = 4;
const ZWJ = 5;
const REGIONAL_INDICATOR = 6;
const PREPEND = 7;
const SPACING_MARK = 8;
const HANGUL_L = 9;
const HANGUL_V = 10;
const HANGUL_T = 11;
const HANGUL_LV = 12;
const HANGUL_LVT = 13;
const EXTENDED_PICTOGRAPHIC = 1 << 4;
const INCB_MASK = 3 << 5;
const INCB_LINKER = 1 << 5;
const INCB_CONSONANT = 2 << 5;
const INCB_EXTEND = 3 << 5;

const EMOJI_NONE = 0;
const EMOJI_PICTOGRAPHIC = 1;
const EMOJI_JOINED = 2;

const hangulFollowers: readonly number[] = [
	0,
	0,
	0,
	0,
	0,
	0,
	0,
	0,
	0,
	(1 << HANGUL_L) | (1 << HANGUL_V) | (1 << HANGUL_LV) | (1 << HANGUL_LVT),
	(1 << HANGUL_V) | (1 << HANGUL_T),
	1 << HANGUL_T,
	(1 << HANGUL_V) | (1 << HANGUL_T),
	1 << HANGUL_T,
];

interface Cluster {
	lastClass: number;
	conjunctLinked: boolean;
	emoji: number;
	regionalIndicators: number;
}

function propertyOf(cp: number): number {
	if (cp < 0 || cp > 0x10ffff) return 0;
	const target = (cp + 1) << paletteBits;
	let lo = 0;
	let hi = scalarRuns.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if ((scalarRuns[mid] ?? 0) < target) lo = mid + 1;
		else hi = mid;
	}
	if (lo === 0) return 0;
	const palette = (scalarRuns[lo - 1] ?? 0) & ((1 << paletteBits) - 1);
	return (scalarPalette[palette] ?? [0, 0, 0])[0] ?? 0;
}

function scalarAt(text: string, index: number): number {
	return text.codePointAt(index) ?? 0;
}

function scalarWidth(scalar: number): number {
	return scalar > 0xffff ? 2 : 1;
}

function scalarStart(text: string, index: number): number {
	const unit = text.charCodeAt(index);
	const previous = text.charCodeAt(index - 1);
	const splitsPair =
		within(unit, 0xdc00, 0xdfff) && within(previous, 0xd800, 0xdbff);
	return splitsPair ? index - 1 : index;
}

function within(unit: number, low: number, high: number): boolean {
	return unit >= low && unit <= high;
}

function isControlClass(kind: number): boolean {
	return kind === CONTROL || kind === CR || kind === LF;
}

function joinsPair(previous: number, next: number): boolean {
	if (previous === CR) return next === LF;
	if (isControlClass(previous) || isControlClass(next)) return false;
	if (((hangulFollowers[previous] ?? 0) & (1 << next)) !== 0) return true;
	return (
		next === EXTEND ||
		next === ZWJ ||
		next === SPACING_MARK ||
		previous === PREPEND
	);
}

function startsWithoutContext(property: number): boolean {
	return (
		(property & EXTENDED_PICTOGRAPHIC) === 0 &&
		(property & INCB_MASK) !== INCB_CONSONANT &&
		(property & CLASS_MASK) !== REGIONAL_INDICATOR
	);
}

function emojiAfter(emoji: number, property: number): number {
	if ((property & EXTENDED_PICTOGRAPHIC) !== 0) return EMOJI_PICTOGRAPHIC;
	if (emoji !== EMOJI_PICTOGRAPHIC) return EMOJI_NONE;
	const kind = property & CLASS_MASK;
	if (kind === EXTEND) return EMOJI_PICTOGRAPHIC;
	return kind === ZWJ ? EMOJI_JOINED : EMOJI_NONE;
}

function extendCluster(cluster: Cluster, property: number): void {
	const conjunct = property & INCB_MASK;
	const kind = property & CLASS_MASK;
	cluster.conjunctLinked =
		conjunct === INCB_LINKER ||
		(cluster.conjunctLinked && conjunct === INCB_EXTEND);
	cluster.emoji = emojiAfter(cluster.emoji, property);
	cluster.regionalIndicators =
		kind === REGIONAL_INDICATOR ? cluster.regionalIndicators + 1 : 0;
	cluster.lastClass = kind;
}

function continuesCluster(cluster: Cluster, next: number): boolean {
	if (cluster.conjunctLinked && (next & INCB_MASK) === INCB_CONSONANT) {
		return true;
	}
	if (cluster.emoji === EMOJI_JOINED && (next & EXTENDED_PICTOGRAPHIC) !== 0) {
		return true;
	}
	return (
		(next & CLASS_MASK) === REGIONAL_INDICATOR &&
		cluster.regionalIndicators % 2 === 1
	);
}

function clusterEnd(text: string, start: number): number {
	const cluster: Cluster = {
		lastClass: 0,
		conjunctLinked: false,
		emoji: EMOJI_NONE,
		regionalIndicators: 0,
	};
	const first = scalarAt(text, start);
	extendCluster(cluster, propertyOf(first));
	let end = start + scalarWidth(first);
	while (end < text.length) {
		const scalar = scalarAt(text, end);
		const next = propertyOf(scalar);
		const joined =
			joinsPair(cluster.lastClass, next & CLASS_MASK) ||
			continuesCluster(cluster, next);
		if (!joined) break;
		extendCluster(cluster, next);
		end += scalarWidth(scalar);
	}
	return end;
}

function guaranteedBoundary(text: string, index: number): number {
	let position = scalarStart(text, index);
	while (position > 0) {
		const next = propertyOf(scalarAt(text, position));
		const before = scalarStart(text, position - 1);
		const previous = propertyOf(scalarAt(text, before)) & CLASS_MASK;
		if (startsWithoutContext(next) && !joinsPair(previous, next & CLASS_MASK)) {
			return position;
		}
		position = before;
	}
	return 0;
}

function clusterAt(text: string, index: number): TextRange {
	let start = guaranteedBoundary(text, index);
	let end = clusterEnd(text, start);
	while (end <= index) {
		start = end;
		end = clusterEnd(text, start);
	}
	return { start, end };
}

export function deletionRange(
	text: string,
	caret: number,
	direction: "backward" | "forward",
): TextRange | null {
	const index = direction === "backward" ? caret - 1 : caret;
	if (index < 0 || index >= text.length) return null;
	return clusterAt(text, index);
}

function deletionDirection(key: string): "backward" | "forward" | null {
	if (key === "Backspace") return "backward";
	return key === "Delete" ? "forward" : null;
}

export function selectDeletedCluster(
	event: KeyboardEvent,
	field: TextField,
): void {
	const direction = deletionDirection(event.key);
	if (direction === null || isImeKey(event)) return;
	if (event.altKey || event.ctrlKey || event.metaKey) return;
	if (event.key === "Delete" && event.shiftKey) return;
	const caret = field.selectionStart;
	if (caret === null || caret !== field.selectionEnd) return;
	const range = deletionRange(field.value, caret, direction);
	if (range !== null) field.setSelectionRange(range.start, range.end);
}
