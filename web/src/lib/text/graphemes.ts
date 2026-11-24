import { propRanges } from "./tables";

const CR = 1;
const LF = 2;
const Control = 3;
const Extend = 4;
const ZWJ = 5;
const RI = 6;
const Prepend = 7;
const SpacingMark = 8;
const L = 9;
const V = 10;
const T = 11;
const LV = 12;
const LVT = 13;

const InCBLinker = 1;
const InCBConsonant = 2;
const InCBExtend = 3;

const controls = (1 << CR) | (1 << LF) | (1 << Control);
const attachNext = (1 << Extend) | (1 << ZWJ) | (1 << SpacingMark);
const hangulNext: Readonly<Record<number, number>> = {
	[L]: (1 << L) | (1 << V) | (1 << LV) | (1 << LVT),
	[V]: (1 << V) | (1 << T),
	[LV]: (1 << V) | (1 << T),
	[T]: 1 << T,
	[LVT]: 1 << T,
};

const range = (i: number): number => propRanges[i] ?? 0;

function packed(cp: number): number {
	let lo = 0;
	let hi = propRanges.length / 3;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (range(mid * 3 + 1) < cp) lo = mid + 1;
		else hi = mid;
	}
	return lo * 3 < propRanges.length && range(lo * 3) <= cp
		? range(lo * 3 + 2)
		: 0;
}

class Segmenter {
	private started = false;
	private prev = 0;
	private riRun = 0;
	private pict = 0;
	private linker = false;

	breaks(cp: number): boolean {
		const value = packed(cp);
		const cur = value & 0x0f;
		const pict = (value & 0x10) !== 0;
		const incb = (value >> 5) & 0x03;
		const brk = this.started ? this.rule(cur, pict, incb) : true;
		this.started = true;
		this.advance(cur, pict, incb);
		return brk;
	}

	private rule(cur: number, pict: boolean, incb: number): boolean {
		const prev = this.prev;
		if (prev === CR && cur === LF) return false;
		if ((controls & ((1 << prev) | (1 << cur))) !== 0) return true;
		if (((hangulNext[prev] ?? 0) & (1 << cur)) !== 0) return false;
		if ((attachNext & (1 << cur)) !== 0 || prev === Prepend) return false;
		if (this.linker && incb === InCBConsonant) return false;
		if (this.pict === 2 && pict) return false;
		if (prev === RI && cur === RI) return this.riRun % 2 === 0;
		return true;
	}

	private advance(cur: number, pict: boolean, incb: number): void {
		this.riRun = cur === RI ? this.riRun + 1 : 0;
		if (pict) this.pict = 1;
		else if (this.pict === 1 && cur === ZWJ) this.pict = 2;
		else if (this.pict !== 1 || cur !== Extend) this.pict = 0;
		if (incb !== InCBExtend) this.linker = incb === InCBLinker;
		this.prev = cur;
	}
}

function scalarAt(text: string, i: number): number {
	return text.codePointAt(i) ?? 0;
}

export function graphemeBoundaries(text: string): number[] {
	const out = [0];
	const seg = new Segmenter();
	for (let i = 0; i < text.length; ) {
		const cp = scalarAt(text, i);
		if (seg.breaks(cp) && i > 0) out.push(i);
		i += cp > 0xffff ? 2 : 1;
	}
	if (text.length > 0) out.push(text.length);
	return out;
}

export function nextBoundary(text: string, i: number): number {
	if (i >= text.length) return text.length;
	const seg = new Segmenter();
	for (let j = i; j < text.length; ) {
		const cp = scalarAt(text, j);
		if (seg.breaks(cp) && j > i) return j;
		j += cp > 0xffff ? 2 : 1;
	}
	return text.length;
}

function restartPoint(text: string, end: number): number {
	for (let j = end - 1; j > 0; j--) {
		const unit = text.charCodeAt(j - 1);
		const gcb = packed(unit) & 0x0f;
		if ((unit < 0xd800 || unit > 0xdfff) && (gcb === LF || gcb === Control))
			return j;
	}
	return 0;
}

export function previousBoundary(text: string, i: number): number {
	if (i <= 0) return 0;
	const end = Math.min(i, text.length);
	let last = restartPoint(text, end);
	for (let b = last; b < end; ) {
		const next = nextBoundary(text, b);
		if (next >= end) break;
		last = next;
		b = next;
	}
	return last;
}
