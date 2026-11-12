import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import App from "#lib/components/App.svelte";
import { type RpcPort, Session } from "#lib/session.svelte.ts";
import type { ApprovalRequest, RpcHandlers } from "#lib/types.ts";

function idlePort(): RpcPort {
	return {
		call: () => Promise.reject(new Error("no call in this test")),
		notify: (): void => undefined,
		respond: (): void => undefined,
		open: () => Promise.resolve(),
		close: (): void => undefined,
	};
}

function sessionWithRequestElsewhere(): Session {
	const dial = (_url: string, _handlers: RpcHandlers): RpcPort => idlePort();
	const session = new Session(dial);
	const request: ApprovalRequest = {
		id: "req-1",
		method: "item/commandExecution/requestApproval",
		threadId: "thread-other",
		turnId: "turn-1",
		itemId: "item-1",
		title: "Run command",
		detail: "ls",
		reason: null,
	};
	session.approvals = [request];
	return session;
}

describe("requests elsewhere", () => {
	afterEach(() => {
		cleanup();
	});

	test("shows the waiting note inside main and opens that thread", async () => {
		const session = sessionWithRequestElsewhere();
		const select = vi
			.spyOn(session, "selectThread")
			.mockResolvedValue(undefined);
		render(App, { props: { session } });
		const note = screen.getByText("1 request waiting on another thread.");
		expect(note.closest("main")).not.toBeNull();
		const user = userEvent.setup();
		await user.click(screen.getByRole("button", { name: "Open thread" }));
		expect(select).toHaveBeenCalledWith("thread-other");
	});
});
