import { afterEach, describe, expect, test } from "vitest";
import { applyTheme, installTokens } from "./theme";

class MemoryStorage implements Storage {
	private readonly values = new Map<string, string>();

	get length(): number {
		return this.values.size;
	}

	clear(): void {
		this.values.clear();
	}

	getItem(key: string): string | null {
		return this.values.get(key) ?? null;
	}

	key(index: number): string | null {
		return [...this.values.keys()][index] ?? null;
	}

	removeItem(key: string): void {
		this.values.delete(key);
	}

	setItem(key: string, value: string): void {
		this.values.set(key, value);
	}
}

describe("applyTheme", () => {
	afterEach(() => {
		document.documentElement.removeAttribute("data-theme");
		document.querySelector('meta[name="theme-color"]')?.remove();
		document.getElementById("dalgona-tokens")?.remove();
	});

	test("applyTheme sets data-theme, caches the setting and updates theme-color", () => {
		const storage = new MemoryStorage();
		installTokens(document);
		applyTheme("soft-night", document, storage);
		expect(document.documentElement.getAttribute("data-theme")).toBe(
			"soft-night",
		);
		expect(storage.getItem("dalgona.theme")).toBe("soft-night");
		const meta = document.querySelector<HTMLMetaElement>(
			'meta[name="theme-color"]',
		);
		expect(meta?.content).toBe("#212225");
	});

	test("installTokens adds one token style element", () => {
		installTokens(document);
		installTokens(document);
		expect(document.querySelectorAll("#dalgona-tokens")).toHaveLength(1);
	});
});
