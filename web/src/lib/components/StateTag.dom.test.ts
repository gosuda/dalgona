import { cleanup, render } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import StateTag from "#lib/components/StateTag.svelte";
import { appliedRule } from "#lib/css-rule.ts";

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

function declared(selector: string, property: string): string {
	return appliedRule(selector).style.getPropertyValue(property);
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

	test("counts the rate-limit wait down locally and holds zero until the server clears it", async () => {
		const view = render(StateTag, {
			props: {
				state: "inProgress",
				startedAtMs: Date.now(),
				point: false,
				retryRemainingMs: 125000,
			},
		});
		await tick();
		expect(elapsed(view.container)).toBe(
			"rate limited · retrying in 2m 05s · esc stops",
		);
		vi.advanceTimersByTime(3000);
		await tick();
		expect(elapsed(view.container)).toBe(
			"rate limited · retrying in 2m 02s · esc stops",
		);
		vi.advanceTimersByTime(125000);
		await tick();
		expect(elapsed(view.container)).toBe(
			"rate limited · retrying in 0m 00s · esc stops",
		);
		await view.rerender({ retryRemainingMs: null });
		await tick();
		expect(elapsed(view.container)).toBe("2m 08s");
	});

	test("a repeated wait report re-arms the countdown from the new value", async () => {
		const view = render(StateTag, {
			props: {
				state: "inProgress",
				startedAtMs: Date.now(),
				point: false,
				retryRemainingMs: 125000,
			},
		});
		await tick();
		vi.advanceTimersByTime(3000);
		await tick();
		expect(elapsed(view.container)).toBe(
			"rate limited · retrying in 2m 02s · esc stops",
		);
		await view.rerender({ retryRemainingMs: 90000 });
		await tick();
		expect(elapsed(view.container)).toBe(
			"rate limited · retrying in 1m 30s · esc stops",
		);
		await view.rerender({ retryRemainingMs: null });
		await tick();
		expect(elapsed(view.container)).toBe("3s");
	});

	test("a tag follows the Status tag row of DESIGN.md", () => {
		render(StateTag, { props: { state: "completed" } });
		expect(declared(".tag", "min-height")).toBe("var(--tag-height)");
		expect(declared(".tag", "min-width")).toBe("var(--tag-min-width)");
		expect(declared(".tag", "padding")).toBe("0px var(--space-2)");
		expect(declared(".tag", "gap")).toBe("var(--space-2)");
		expect(declared(".tag", "border-radius")).toBe("var(--radius-tag)");
		expect(declared(".tag", "font-size")).toBe("var(--size-small)");
		expect(declared(".tag", "font-weight")).toBe("var(--weight-control)");
		expect(declared(".tag", "line-height")).toBe("var(--leading-tight)");
		expect(declared(".tag", "white-space")).toBe("nowrap");
		expect(declared(".tag", "color")).toBe("var(--dim)");
	});

	test("the live tag keeps the point fill and ink, and only its counter may wrap", () => {
		render(StateTag, {
			props: { state: "inProgress", startedAtMs: Date.now(), point: true },
		});
		expect(declared(".tag.point", "background")).toBe("var(--point)");
		expect(declared(".tag.point", "color")).toBe("var(--on-point)");
		expect(declared(".tag.point", "border")).toBe("0px");
		expect(declared(".elapsed", "white-space")).toBe("normal");
	});

	test("Offline is the only tag in warning-text", () => {
		const offline = render(StateTag, { props: { state: "disconnected" } });
		const tag = offline.container.querySelector(".tag");
		expect(tag?.textContent).toBe("Offline");
		expect(tag?.classList.contains("offline")).toBe(true);
		expect(declared(".tag.offline", "color")).toBe("var(--warning-text)");
		offline.unmount();

		for (const state of [
			"idle",
			"completed",
			"interrupted",
			"awaitingApproval",
		] as const) {
			const view = render(StateTag, { props: { state } });
			expect(view.container.querySelector(".offline")).toBeNull();
			view.unmount();
		}
	});

	test("Failed is the bold failed-text word in the title face on one line of its own size", () => {
		render(StateTag, { props: { state: "failed" } });
		expect(declared(".failed", "color")).toBe("var(--failed-text)");
		expect(declared(".failed", "font-weight")).toBe("var(--weight-strong)");
		expect(declared(".failed", "font-size")).toBe("var(--size-small)");
		expect(declared(".failed", "line-height")).toBe("var(--leading-tight)");
		expect(declared(".failed", "font-family")).toBe("var(--face-title)");
	});
});
