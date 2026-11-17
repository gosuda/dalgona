import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import { scale } from "./scale";
import { tokenCss } from "./theme";

interface DesignRow {
	readonly cells: readonly string[];
}

interface DesignTable {
	readonly heading: string;
	readonly columns: readonly string[];
	readonly rows: readonly DesignRow[];
}

const designMarkdown = readFileSync(
	fileURLToPath(new URL("../../../../DESIGN.md", import.meta.url)),
	"utf8",
);

const NUMBER = /(\d+(?:\.\d+)?)(?:\s*)(px|ms|s|ch|rem|em|%)?/g;
const TOLERANCE = 0.000001;

function isTableLine(line: string): boolean {
	return line.trimStart().startsWith("|");
}

function isSeparatorRow(cells: readonly string[]): boolean {
	return cells.every((cell) => /^:?-+:?$/.test(cell));
}

function splitRow(line: string): string[] {
	const trimmed = line.trim();
	const body = trimmed.startsWith("|") ? trimmed.slice(1) : trimmed;
	const end =
		body.endsWith("|") && !body.endsWith("\\|") ? body.length - 1 : body.length;
	const cells: string[] = [];
	let current = "";
	let inTicks = false;
	for (let index = 0; index < end; index += 1) {
		const char = body[index];
		if (char === "\\" && body[index + 1] === "|") {
			current += "|";
			index += 1;
			continue;
		}
		if (char === "`") {
			inTicks = !inTicks;
			continue;
		}
		if (char === "|" && !inTicks) {
			cells.push(current);
			current = "";
			continue;
		}
		current += char;
	}
	cells.push(current);
	return cells.map((cell) => {
		const trimmedCell = cell.trim();
		return trimmedCell.startsWith("`") && trimmedCell.endsWith("`")
			? trimmedCell.slice(1, -1)
			: trimmedCell;
	});
}

function parseDesignTables(markdown: string): readonly DesignTable[] {
	const tables: { heading: string; table: DesignTable }[] = [];
	let heading = "";
	const lines = markdown.split("\n");
	for (let index = 0; index < lines.length; index += 1) {
		const line = lines[index] as string;
		const headingMatch = line.match(/^#{1,6}\s+(.*\S)\s*$/);
		if (headingMatch) {
			heading = headingMatch[1] ?? "";
			continue;
		}
		if (!isTableLine(line)) {
			continue;
		}
		const block: string[] = [];
		while (index < lines.length && isTableLine(lines[index] as string)) {
			block.push(lines[index] as string);
			index += 1;
		}
		index -= 1;
		const [header, separator, ...dataRows] = block;
		if (
			header === undefined ||
			separator === undefined ||
			dataRows.length === 0
		) {
			throw new Error(
				`malformed table under "${heading}": needs a header, a separator and at least one row`,
			);
		}
		const columns = splitRow(header);
		if (!isSeparatorRow(splitRow(separator))) {
			throw new Error(
				`malformed table under "${heading}": missing separator row`,
			);
		}
		if (columns.some((column) => column === "")) {
			throw new Error(`malformed table under "${heading}": empty header cell`);
		}
		const rows: DesignRow[] = [];
		for (const rowLine of dataRows) {
			const cells = splitRow(rowLine);
			if (cells.length !== columns.length) {
				throw new Error(
					`malformed table under "${heading}": row has ${cells.length} cells, expected ${columns.length}`,
				);
			}
			if (cells.some((cell) => cell === "")) {
				throw new Error(`malformed table under "${heading}": empty data cell`);
			}
			rows.push({ cells });
		}
		if (rows.length === 0) {
			throw new Error(`malformed table under "${heading}": no data rows`);
		}
		tables.push({
			heading,
			table: { heading, columns, rows },
		});
	}
	const identities = new Set<string>();
	for (const { table } of tables) {
		const identity = `${table.heading}\n${table.columns.join("\n")}`;
		if (identities.has(identity)) {
			throw new Error(`duplicate table identity under "${table.heading}"`);
		}
		identities.add(identity);
	}
	return tables.map(({ table }) => table);
}

function kebab(name: string): string {
	return name.replace(/([a-z])([A-Z0-9])/g, "$1-$2").toLowerCase();
}

function findTable(
	tables: readonly DesignTable[],
	heading: string,
): DesignTable {
	const matches = tables.filter((table) => table.heading === heading);
	if (matches.length !== 1) {
		throw new Error(
			`expected exactly one table under "${heading}", found ${matches.length}`,
		);
	}
	return matches[0] as DesignTable;
}

function unitMatches(found: string, expected: string): boolean {
	if (found === expected) return true;
	if (
		(found === "" || found === "px") &&
		(expected === "" || expected === "px")
	) {
		return true;
	}
	return false;
}

function columnIndex(table: DesignTable, name: string): number {
	const index = table.columns.indexOf(name);
	if (index === -1) {
		throw new Error(`table under "${table.heading}" has no "${name}" column`);
	}
	return index;
}

function cellAt(table: DesignTable, row: DesignRow, name: string): string {
	const value = row.cells[columnIndex(table, name)];
	if (value === undefined) {
		throw new Error(`table under "${table.heading}" row missing "${name}"`);
	}
	return value;
}
function numbersIn(text: string): { value: number; unit: string }[] {
	const found: { value: number; unit: string }[] = [];
	for (const match of text.matchAll(NUMBER)) {
		const unit = match[2] ?? "";
		found.push({ value: Number(match[1]), unit });
	}
	return found;
}

function valuesEqual(found: number, expected: number): boolean {
	return Math.abs(found - expected) <= TOLERANCE;
}

function extractFromCell(cell: string, expected: ParsedTokenValue): boolean {
	if (expected.ratio !== undefined) {
		return cell.replace(/\s+/g, "").includes(expected.ratio);
	}
	return numbersIn(cell).some(
		(found) =>
			valuesEqual(found.value, expected.value) &&
			unitMatches(found.unit, expected.unit),
	);
}

interface ParsedTokenValue {
	readonly value: number;
	readonly unit: string;
	readonly raw: string;
	readonly literal?: string;
	readonly ratio?: string;
}

function parseTokenValue(raw: string): ParsedTokenValue {
	const trimmed = raw.trim();
	const ratioMatch = trimmed.match(
		/^(-?\d+(?:\.\d+)?)\s*\/\s*(-?\d+(?:\.\d+)?)$/,
	);
	if (ratioMatch !== null) {
		const numerator = Number(ratioMatch[1]);
		const denominator = Number(ratioMatch[2]);
		if (denominator === 0) {
			throw new Error(`cannot parse token value "${raw}": zero denominator`);
		}
		return {
			value: numerator / denominator,
			unit: "",
			raw: trimmed,
			ratio: `${ratioMatch[1]}/${ratioMatch[2]}`,
		};
	}
	const numeric = trimmed.match(/^(-?\d+(?:\.\d+)?)(px|ms|s|ch|rem|em|%)?$/);
	if (numeric !== null) {
		return {
			value: Number(numeric[1]),
			unit: numeric[2] ?? "",
			raw: trimmed,
		};
	}
	const found = numbersIn(trimmed);
	const [first] = found;
	if (
		first !== undefined &&
		found.length === 1 &&
		trimmed === `${first.value}${first.unit}`
	) {
		return { value: first.value, unit: first.unit, raw: trimmed };
	}
	if (/^[a-zA-Z-]+$/.test(trimmed)) {
		return { value: 0, unit: "", raw: trimmed, literal: trimmed };
	}
	throw new Error(`cannot parse token value "${raw}"`);
}

function tokenValueEquals(
	found: ParsedTokenValue,
	expected: ParsedTokenValue,
): boolean {
	if (found.literal !== undefined || expected.literal !== undefined) {
		return (
			found.literal !== undefined &&
			expected.literal !== undefined &&
			found.literal.toLowerCase() === expected.literal.toLowerCase()
		);
	}
	return (
		found.unit === expected.unit &&
		Math.abs(found.value - expected.value) <= TOLERANCE
	);
}

const tables = parseDesignTables(designMarkdown);
const scaleTable = findTable(tables, "Scale");
const bindingsTable = findTable(tables, "Component token bindings");

const scaleRows = scaleTable.rows.map((row) => ({
	token: cellAt(scaleTable, row, "Token"),
	property: cellAt(scaleTable, row, "CSS custom property"),
	value: cellAt(scaleTable, row, "Value"),
}));

const bindingRows = bindingsTable.rows.map((row) => ({
	token: cellAt(bindingsTable, row, "Token"),
	property: cellAt(bindingsTable, row, "CSS property"),
	citation: cellAt(bindingsTable, row, "Existing value or expression"),
}));

const valueTables = tables.filter(
	(table) =>
		table.heading !== "Scale" && table.heading !== "Component token bindings",
);
const partTables = valueTables.filter((table) =>
	table.columns.includes("Part"),
);
for (const used of [scaleTable, bindingsTable, ...partTables]) {
	if (new Set(used.columns).size !== used.columns.length) {
		throw new Error(`duplicate header cells in table under "${used.heading}"`);
	}
}
const BROWSER_TABLE_HEADINGS = new Set(["Browser controls", "Browser cards"]);
const COLUMN_ORDER = [
	"Geometry",
	"Type",
	"Colour",
	"Browser",
	"Terminal",
	"Property",
];

function cellsContaining(
	table: DesignTable,
	cells: readonly string[],
	expected: ParsedTokenValue,
): boolean {
	return COLUMN_ORDER.some((column) => {
		const index = table.columns.indexOf(column);
		const cell = index === -1 ? undefined : cells[index];
		return cell !== undefined && extractFromCell(cell, expected);
	});
}

const componentRowNames = new Set(
	partTables.flatMap((table) =>
		table.rows.map((row) => cellAt(table, row, "Part")),
	),
);

function rowCandidates(
	descriptor: string,
	expected: ParsedTokenValue,
): { table: DesignTable; cells: readonly string[] }[] {
	const named = new Set<string>();
	const collect = (text: string): void => {
		const trimmedText = text.trim();
		if (trimmedText === "") return;
		if (componentRowNames.has(trimmedText)) named.add(trimmedText);
		const comma = trimmedText.lastIndexOf(",");
		if (comma !== -1) collect(trimmedText.slice(0, comma));
		const space = trimmedText.search(/\s+\S+$/);
		if (space !== -1) collect(trimmedText.slice(0, space));
	};
	collect(descriptor);
	if (named.size > 0) {
		const matches: { table: DesignTable; cells: readonly string[] }[] = [];
		for (const name of named) {
			for (const table of partTables) {
				const partIndex = columnIndex(table, "Part");
				for (const row of table.rows) {
					if (row.cells[partIndex] === name) {
						matches.push({ table, cells: row.cells });
					}
				}
			}
		}
		const containing = matches.filter((match) =>
			cellsContaining(match.table, match.cells, expected),
		);
		const pool = containing.length > 0 ? containing : matches;
		const browser = pool.filter((match) =>
			BROWSER_TABLE_HEADINGS.has(match.table.heading),
		);
		return browser.length > 0 ? browser : pool;
	}
	const words = descriptor.toLowerCase().split(/\s+/).filter(Boolean);
	const phrase = words.join(" ");
	const matches: { table: DesignTable; cells: readonly string[] }[] = [];
	for (const table of valueTables) {
		const scope =
			`${table.heading} ${table.rows.map((row) => row.cells.join(" ")).join(" ")}`.toLowerCase();
		if (!words.every((word) => scope.includes(word))) continue;
		if (!scope.includes(phrase)) continue;
		for (const row of table.rows) {
			if (!cellsContaining(table, row.cells, expected)) continue;
			matches.push({ table, cells: row.cells });
		}
	}
	return matches;
}

function resolveBindingValue(
	token: string,
	citation: string,
): ParsedTokenValue {
	const valueMatch = citation.match(/,\s*([^,]+)$/);
	if (valueMatch === null) {
		throw new Error(`binding "${token}" citation has no value: "${citation}"`);
	}
	const valueText = (valueMatch[1] ?? "").trim();
	const expected = parseTokenValue(valueText);
	const descriptor = citation
		.slice(0, citation.length - valueMatch[0].length)
		.trim();
	const candidates = rowCandidates(descriptor, expected);
	if (candidates.length !== 1) {
		throw new Error(
			`binding "${token}" citation resolves to ${candidates.length} rows: "${citation}"`,
		);
	}
	const [candidate] = candidates;
	if (candidate === undefined) {
		throw new Error(
			`binding "${token}" citation resolved no row: "${citation}"`,
		);
	}
	const { table, cells } = candidate;
	for (const name of COLUMN_ORDER) {
		const index = table.columns.indexOf(name);
		const cell = index === -1 ? undefined : cells[index];
		if (cell !== undefined && extractFromCell(cell, expected)) {
			return expected;
		}
	}
	throw new Error(
		`binding "${token}" value ${valueText} not found in row under "${table.heading}"`,
	);
}

const scaleByToken = new Map(scaleRows.map((row) => [row.token, row]));
const bindingByToken = new Map(bindingRows.map((row) => [row.token, row]));
const runtimeScale: Readonly<Record<string, string>> = scale;
const css = tokenCss();

function requireScale(token: string): string {
	const value = runtimeScale[token];
	if (value === undefined) {
		throw new Error(`scale.${token} missing`);
	}
	return value;
}

describe("design contract: Scale table", () => {
	test("every Scale row is an exported token with the table value", () => {
		for (const row of scaleRows) {
			const exported = requireScale(row.token);
			expect(exported, `scale.${row.token} missing`).toBeDefined();
			const expected = parseTokenValue(row.value);
			const found = parseTokenValue(exported);
			expect(
				tokenValueEquals(found, expected),
				`scale.${row.token} value ${exported} != ${row.value}`,
			).toBe(true);
			expect(row.property).toBe(`--${kebab(row.token)}`);
			expect(css).toContain(`${row.property}:${exported};`);
		}
	});

	test("every exported scale token appears in the Scale or bindings table", () => {
		const tableTokens = new Set([
			...scaleByToken.keys(),
			...bindingByToken.keys(),
		]);
		const typeTableTokens = new Set(["faceTitle", "faceText", "faceMono"]);
		for (const token of Object.keys(runtimeScale)) {
			if (typeTableTokens.has(token)) continue;
			expect(
				tableTokens.has(token),
				`scale.${token} has no DESIGN.md token row`,
			).toBe(true);
		}
	});

	test("no duplicate token rows in either table", () => {
		expect(scaleByToken.size).toBe(scaleRows.length);
		expect(bindingByToken.size).toBe(bindingRows.length);
	});
});

describe("design contract: Component token bindings", () => {
	test("every binding resolves to its cited component value", () => {
		for (const row of bindingRows) {
			const expected = resolveBindingValue(row.token, row.citation);
			const exported = requireScale(row.token);
			expect(exported, `scale.${row.token} missing`).toBeDefined();
			const found = parseTokenValue(exported);
			expect(
				tokenValueEquals(found, expected),
				`scale.${row.token} value ${exported} != ${row.citation}`,
			).toBe(true);
			expect(row.property).toBe(`--${kebab(row.token)}`);
			expect(css).toContain(`${row.property}:${exported};`);
		}
	});

	test("negative control: a mutated table cell is detected", () => {
		const mutated = designMarkdown.replace(
			"|`space5`|`--space-5`|Button inline padding and Card padding, 20px|",
			"|`space5`|`--space-5`|Button inline padding and Card padding, 21px|",
		);
		expect(mutated).not.toBe(designMarkdown);
		const mutatedTables = parseDesignTables(mutated);
		const mutatedBindings = findTable(
			mutatedTables,
			"Component token bindings",
		);
		const mutatedRow = mutatedBindings.rows.find(
			(row) => row.cells[0] === "space5",
		);
		expect(mutatedRow).toBeDefined();
		if (mutatedRow === undefined) {
			throw new Error("space5 row missing from mutated bindings");
		}
		const citation = cellAt(
			mutatedBindings,
			mutatedRow,
			"Existing value or expression",
		);
		expect(() => resolveBindingValue("space5", citation)).toThrow(
			/not found in row|component rows/,
		);
	});

	test("negative control: a mutated runtime value is detected", () => {
		const mutatedScale = { ...runtimeScale, space5: "21px" };
		const expected = resolveBindingValue(
			"space5",
			"Button inline padding and Card padding, 20px",
		);
		const found = parseTokenValue(mutatedScale.space5);
		expect(Math.abs(found.value - expected.value)).toBeGreaterThan(TOLERANCE);
	});
});
