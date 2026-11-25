import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import corpus from "../../../../docs/dal/spec/input-text-cases.json";
import { deletionRange } from "./editing";

interface Vector {
	readonly id: string;
	readonly codePoints: readonly string[];
	readonly text: string;
	readonly scalarCount: number;
	readonly graphemeBoundaries: readonly number[];
	readonly policy: string;
}

interface Segmented {
	readonly name: string;
	readonly scalars: readonly number[];
	readonly boundaries: readonly number[];
}

const vectors: readonly Vector[] = (
	corpus as unknown as { vectors: readonly Vector[] }
).vectors;

const graphemeBreakTest = fileURLToPath(
	new URL(
		"../../../../internal/dal/textinput/testdata/GraphemeBreakTest-18.0.0.txt",
		import.meta.url,
	),
);
const graphemeBreakTestSHA256 =
	"b0cf047ee94485bbdc846de2b902f5f8a815f6b674f9d04223cddadd91c9df31";

function segmentedVector(vector: Vector): Segmented {
	return {
		name: vector.id,
		scalars: vector.codePoints.map((hex) => Number.parseInt(hex, 16)),
		boundaries: vector.graphemeBoundaries,
	};
}

function segmentedLine(line: string): Segmented | null {
	const body = line.split("#")[0]?.trim() ?? "";
	if (body === "") return null;
	const scalars: number[] = [];
	const boundaries: number[] = [];
	for (const token of body.split(/\s+/)) {
		if (token === "÷") boundaries.push(scalars.length);
		else if (token !== "×") scalars.push(Number.parseInt(token, 16));
	}
	return { name: body, scalars, boundaries };
}

function unitOffsets(scalars: readonly number[]): readonly number[] {
	const offsets = [0];
	for (const scalar of scalars) {
		offsets.push((offsets.at(-1) ?? 0) + (scalar > 0xffff ? 2 : 1));
	}
	return offsets;
}

function deletionMismatches(segmented: Segmented): readonly string[] {
	const text = String.fromCodePoint(...segmented.scalars);
	const units = unitOffsets(segmented.scalars);
	const mismatches: string[] = [];
	for (let index = 1; index < segmented.boundaries.length; index += 1) {
		const first = segmented.boundaries[index - 1] ?? 0;
		const last = segmented.boundaries[index] ?? 0;
		const expected = JSON.stringify({ start: units[first], end: units[last] });
		for (let scalar = first; scalar <= last; scalar += 1) {
			const caret = units[scalar] ?? 0;
			const backward = JSON.stringify(deletionRange(text, caret, "backward"));
			const forward = JSON.stringify(deletionRange(text, caret, "forward"));
			if (scalar > first && backward !== expected) {
				mismatches.push(`${segmented.name} backward from ${scalar}`);
			}
			if (scalar < last && forward !== expected) {
				mismatches.push(`${segmented.name} forward from ${scalar}`);
			}
		}
	}
	return mismatches;
}

describe("branch cluster deletion", () => {
	test("Backspace and Delete remove the whole cluster at corpus boundaries", () => {
		expect(
			vectors.flatMap((vector) => deletionMismatches(segmentedVector(vector))),
		).toEqual([]);
	});

	test("deletion past either end removes nothing", () => {
		for (const vector of vectors) {
			expect(deletionRange(vector.text, 0, "backward"), vector.id).toBeNull();
			expect(
				deletionRange(vector.text, vector.text.length, "forward"),
				vector.id,
			).toBeNull();
		}
	});
});

describe("branch GraphemeBreakTest deletion", () => {
	const source = readFileSync(graphemeBreakTest);
	const lines = source
		.toString("utf8")
		.split("\n")
		.map(segmentedLine)
		.filter((line) => line !== null);

	test("the vendored test file matches its pin", () => {
		expect(createHash("sha256").update(source).digest("hex")).toBe(
			graphemeBreakTestSHA256,
		);
		expect(lines).toHaveLength(853);
	});

	test("Backspace and Delete remove the whole cluster on every line", () => {
		expect(lines.flatMap(deletionMismatches)).toEqual([]);
	});
});
