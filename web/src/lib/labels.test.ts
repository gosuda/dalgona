import { describe, expect, test } from "vitest";
import { countLabel, stateKey, stateWords } from "./labels";

describe("countLabel", () => {
	test("countLabel uses singular and plural forms", () => {
		expect(countLabel(1, "request", "requests")).toBe("1 request");
		expect(countLabel(2, "request", "requests")).toBe("2 requests");
		expect(countLabel(0, "request", "requests")).toBe("0 requests");
	});
});

describe("state words", () => {
	test("uses the state words of this step", () => {
		expect(stateWords.inProgress).toBe("Working");
		expect(stateWords.awaitingApproval).toBe("Waiting");
		expect(stateWords.completed).toBe("Done");
		expect(stateWords.failed).toBe("Failed");
		expect(stateWords.declined).toBe("Declined");
		expect(stateWords.interrupted).toBe("Stopped");
		expect(stateWords.idle).toBe("Ready");
		expect(stateWords.disconnected).toBe("Offline");
	});

	test("maps a thread that is working to Working", () => {
		expect(stateKey("active")).toBe("inProgress");
	});

	test("maps a thread waiting on a user answer or an approval to Waiting", () => {
		expect(stateKey("active", ["waitingOnUserInput"])).toBe("awaitingApproval");
		expect(stateKey("active", ["waitingOnApproval"])).toBe("awaitingApproval");
		expect(stateKey("active", [])).toBe("inProgress");
	});

	test("maps an idle thread to Ready", () => {
		expect(stateKey("idle")).toBe("idle");
	});

	test("returns null for a status it does not know", () => {
		expect(stateKey("custom")).toBeNull();
	});
});
