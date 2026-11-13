import { cleanup, render } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import StateTag from "#lib/components/StateTag.svelte";

class MotionPreference extends EventTarget {
	readonly media = "(prefers-reduced-motion: reduce)";
	matches: boolean;

	constructor(reduced: boolean) {
		super();
		this.matches = reduced;
	}

	prefer(reduced: boolean): void {
		this.matches = reduced;
		this.dispatchEvent(new Event("change"));
	}
}

function stubMotion(reduced: boolean): MotionPreference {
	const preference = new MotionPreference(reduced);
	vi.stubGlobal("matchMedia", () => preference);
	return preference;
}

function elapsed(container: HTMLElement): string {
	return container.querySelector(".elapsed")?.textContent ?? "";
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
		const { container } = render(StateTag, {
			props: { state: "inProgress", startedAtMs: Date.now(), point: false },
		});
		await tick();
		vi.advanceTimersByTime(3000);
		await tick();
		expect(elapsed(container)).toBe("0 ms");
	});

	test("the counter follows a motion preference that changes after it mounts", async () => {
		const motion = stubMotion(false);
		const { container } = render(StateTag, {
			props: {
				state: "inProgress",
				startedAtMs: Date.now() - 500,
				point: false,
			},
		});
		await tick();
		expect(elapsed(container)).toBe("500 ms");
		vi.advanceTimersByTime(2900);
		await tick();
		expect(elapsed(container)).toBe("2s");

		motion.prefer(true);
		await tick();
		expect(elapsed(container)).toBe("2s");
		vi.advanceTimersByTime(2000);
		await tick();
		expect(elapsed(container)).toBe("2s");

		motion.prefer(false);
		await tick();
		expect(elapsed(container)).toBe("5s");
		vi.advanceTimersByTime(2000);
		await tick();
		expect(elapsed(container)).toBe("7s");
	});

	test("a new item refreshes the held counter after the preference turns to reduce", async () => {
		const motion = stubMotion(false);
		const view = render(StateTag, {
			props: {
				state: "inProgress",
				startedAtMs: Date.now(),
				tick: 0,
				point: false,
			},
		});
		await tick();
		motion.prefer(true);
		await tick();
		vi.advanceTimersByTime(3000);
		await tick();
		expect(elapsed(view.container)).toBe("0 ms");

		await view.rerender({ tick: 1 });
		await tick();
		expect(elapsed(view.container)).toBe("3s");
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
