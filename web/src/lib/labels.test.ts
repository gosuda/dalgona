import { describe, expect, test } from "vitest";
import { countLabel } from "./labels";

describe("countLabel", () => {
	test("countLabel uses singular and plural forms", () => {
		expect(countLabel(1, "request", "requests")).toBe("1 request");
		expect(countLabel(2, "request", "requests")).toBe("2 requests");
		expect(countLabel(0, "request", "requests")).toBe("0 requests");
	});
});
