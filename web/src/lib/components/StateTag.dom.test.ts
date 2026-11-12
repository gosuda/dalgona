import { cleanup, render } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import StateTag from "#lib/components/StateTag.svelte";
import StateTagStage from "#lib/components/StateTagStage.svelte";

function stubMotion(reduced: boolean): void {
	vi.stubGlobal("matchMedia", (query: string) => ({
		matches: reduced && query.includes("reduce"),
		media: query,
		addEventListener: () => {},
		removeEventListener: () => {},
	}));
}

describe("StateTag", () => {
	beforeEach(() => {
		vi.useFakeTimers();
		stubMotion(false);
	});

	afterEach(() => {
		cleanup();
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});

	test("only the point is filled, and Failed is bold text", () => {
		const { container, unmount } = render(StateTag, {
			props: { state: "inProgress", startedAtMs: Date.now(), point: true },
		});
		expect(container.querySelector(".tag.point")).not.toBeNull();
		unmount();

		const plain = render(StateTag, {
			props: { state: "inProgress", startedAtMs: Date.now(), point: false },
		});
		expect(plain.container.querySelector(".point")).toBeNull();
		plain.unmount();

		const failed = render(StateTag, { props: { state: "failed" } });
		const word = failed.container.querySelector(".failed");
		expect(word?.textContent).toBe("Failed");
		expect(word?.classList.contains("point")).toBe(false);
	});

	test("the elapsed counter ticks every second", async () => {
		const start = Date.now();
		const { container } = render(StateTag, {
			props: { state: "inProgress", startedAtMs: start, point: false },
		});
		await tick();
		expect(container.textContent).toContain("0 ms");
		vi.advanceTimersByTime(3000);
		await tick();
		expect(container.textContent).toContain("3s");
	});

	test("the counter does not tick under reduced motion", async () => {
		stubMotion(true);
		const start = Date.now();
		const { container } = render(StateTag, {
			props: { state: "inProgress", startedAtMs: start, point: false },
		});
		vi.advanceTimersByTime(3000);
		expect(container.textContent).toContain("0 ms");
		expect(container.textContent).not.toContain("3s");
	});

	test("under reduced motion the counter reads the clock again when tick changes", async () => {
		stubMotion(true);
		const start = Date.now();
		const { container } = render(StateTagStage, {
			props: { startedAtMs: start },
		});
		await tick();
		vi.setSystemTime(start + 3000);
		container.querySelector("button")?.click();
		await tick();
		expect(container.textContent).toContain("3s");
	});

	test("shows the rate-limit countdown in place of the elapsed time", async () => {
		const { container } = render(StateTag, {
			props: {
				state: "inProgress",
				startedAtMs: Date.now(),
				point: false,
				retryRemainingMs: 125000,
			},
		});
		await tick();
		expect(container.textContent).toContain(
			"rate limited · retrying in 2m 05s · esc stops",
		);
		expect(container.textContent).not.toContain("0 ms");
	});
});
