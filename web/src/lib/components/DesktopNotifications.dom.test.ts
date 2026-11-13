import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import DesktopNotifications from "#lib/components/DesktopNotifications.svelte";

describe("desktop notifications row", () => {
	afterEach(cleanup);

	test.each([
		["on", "Desktop notifications are on"],
		["blocked", "Desktop notifications are blocked in this browser"],
		["off", "Desktop notifications are off"],
	] as const)("shows the text of the %s state", (value, text) => {
		render(DesktopNotifications, { props: { value, onAllow: vi.fn() } });
		expect(screen.getByText(text)).toBeTruthy();
	});

	test("offers the allow button only when the state is off, and calls onAllow", async () => {
		const onAllow = vi.fn();
		render(DesktopNotifications, { props: { value: "off", onAllow } });
		await fireEvent.click(
			screen.getByRole("button", { name: "Allow desktop notifications" }),
		);
		expect(onAllow).toHaveBeenCalledTimes(1);
	});

	test.each(["on", "blocked"] as const)(
		"offers no button in the %s state",
		(value) => {
			render(DesktopNotifications, { props: { value, onAllow: vi.fn() } });
			expect(screen.queryByRole("button")).toBeNull();
		},
	);

	test("renders no row when notifications are unsupported", () => {
		const { container } = render(DesktopNotifications, {
			props: { value: "unsupported", onAllow: vi.fn() },
		});
		expect(container.querySelector("p")).toBeNull();
	});
});
