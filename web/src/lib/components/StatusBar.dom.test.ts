import { cleanup, render } from "@testing-library/svelte";
import { afterEach, describe, expect, test, vi } from "vitest";
import StatusBar from "#lib/components/StatusBar.svelte";
import type { StatusInfo } from "#lib/types.ts";

const status: StatusInfo = {
	model: "gpt-test",
	approval: "auto",
	sandbox: "workspace-write",
	runner: "local",
};

function props(activeFlags: readonly string[]) {
	return {
		status,
		connection: "connected" as const,
		advisor: { kind: "unknown" as const },
		turnStatus: "inProgress" as const,
		turnStartedAtMs: Date.now(),
		retryRemainingMs: null,
		point: false,
		activeFlags,
		revision: 0,
		onReconnect: vi.fn(),
	};
}

describe("StatusBar turn tag", () => {
	afterEach(() => {
		cleanup();
	});

	test("shows Waiting while the open thread waits on an approval", () => {
		const { container } = render(StatusBar, {
			props: props(["waitingOnApproval"]),
		});
		expect(container.textContent).toContain("Waiting");
		expect(container.textContent).not.toContain("Working");
	});

	test("shows Waiting while the open thread waits on a user answer", () => {
		const { container } = render(StatusBar, {
			props: props(["waitingOnUserInput"]),
		});
		expect(container.textContent).toContain("Waiting");
	});

	test("shows Working while the open thread runs with no waiting flag", () => {
		const { container } = render(StatusBar, { props: props([]) });
		expect(container.textContent).toContain("Working");
		expect(container.textContent).not.toContain("Waiting");
	});
});
