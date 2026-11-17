import { strict as assert } from "node:assert";
import { createHash, createHmac } from "node:crypto";
import { test } from "node:test";
import {
	DaltokiRelay,
	type DaltokiStorage,
	handleDaltokiRelay,
	handleDaltokiWebhook,
	type RelayEnvelope,
} from "./daltoki.ts";

const WEBHOOK_SECRET = "webhook-secret-for-tests";
const RELAY_TOKEN = "relay-token-for-tests";

type HandlerEnv = Parameters<typeof handleDaltokiWebhook>[1];

function createStorage(): { storage: DaltokiStorage; keys: () => string[] } {
	const data = new Map<string, unknown>();
	return {
		storage: {
			get: async <T>(key: string) => data.get(key) as T | undefined,
			put: async (key: string, value: unknown) => {
				data.set(key, value);
			},
			delete: async (key: string) => data.delete(key),
			list: async <T>({
				prefix,
				startAfter,
				limit,
			}: {
				prefix?: string;
				startAfter?: string;
				limit?: number;
			} = {}) => {
				const out = new Map<string, unknown>();
				const keys = [...data.keys()].sort();
				for (const key of keys) {
					if (prefix !== undefined && !key.startsWith(prefix)) {
						continue;
					}
					if (startAfter !== undefined && key <= startAfter) {
						continue;
					}
					if (out.size >= (limit ?? Number.POSITIVE_INFINITY)) {
						break;
					}
					out.set(key, data.get(key));
				}
				return out as Map<string, T>;
			},
		},
		keys: () => [...data.keys()],
	};
}

function createHarness(): { env: HandlerEnv; relay: DaltokiRelay } {
	const { storage } = createStorage();
	const relay = new DaltokiRelay({ storage });
	const namespace = {
		getByName: (_name: string) => ({
			fetch: (request: Request) => relay.fetch(request),
		}),
	};
	const env = {
		DALTOKI_WEBHOOK_SECRET: WEBHOOK_SECRET,
		DALTOKI_RELAY_TOKEN: RELAY_TOKEN,
		DALTOKI_RELAY: namespace,
	} as unknown as HandlerEnv;
	return { env, relay };
}

function sign(secret: string, body: Uint8Array): string {
	return `sha256=${createHmac("sha256", secret).update(body).digest("hex")}`;
}

function webhookRequest(
	body: Uint8Array,
	options: { secret?: string; delivery?: string; event?: string } = {},
): Request {
	const headers: Record<string, string> = {
		"X-GitHub-Delivery": options.delivery ?? "delivery-1",
		"X-GitHub-Event": options.event ?? "issues",
		"X-Hub-Signature-256": sign(options.secret ?? WEBHOOK_SECRET, body),
		"Content-Type": "application/json",
	};
	return new Request("https://example/daltoki/webhook", {
		method: "POST",
		headers,
		body,
	});
}

function relayRequest(
	path: "/daltoki/relay/pull" | "/daltoki/relay/ack",
	payload: unknown,
	token = RELAY_TOKEN,
): Request {
	return new Request(`https://example${path}`, {
		method: "POST",
		headers: {
			Authorization: `Bearer ${token}`,
			"Content-Type": "application/json",
		},
		body: JSON.stringify(payload),
	});
}

async function pull(
	env: HandlerEnv,
	after = "",
	limit = 100,
): Promise<{ items: RelayEnvelope[]; next: string }> {
	const response = await handleDaltokiRelay(
		relayRequest("/daltoki/relay/pull", { after, limit }),
		env,
	);
	assert.equal(response.status, 200);
	return (await response.json()) as { items: RelayEnvelope[]; next: string };
}

test("webhook intake stores a signed envelope for relay pull", async () => {
	const { env } = createHarness();
	const raw = Buffer.from(JSON.stringify({ action: "opened" }));
	const response = await handleDaltokiWebhook(webhookRequest(raw), env);
	assert.equal(response.status, 202);
	const page = await pull(env);
	assert.equal(page.items.length, 1);
	const item = page.items[0] as RelayEnvelope;
	assert.equal(item.deliveryID, "delivery-1");
	assert.equal(item.event, "issues");
	assert.equal(item.bodySHA256, createHash("sha256").update(raw).digest("hex"));
	assert.equal(Buffer.from(item.body, "base64").toString(), raw.toString());
	assert.equal(page.next, "delivery-1");
});

test("duplicate delivery with identical bytes returns 202 without a second copy", async () => {
	const { env } = createHarness();
	const raw = Buffer.from(JSON.stringify({ action: "opened" }));
	const first = await handleDaltokiWebhook(webhookRequest(raw), env);
	const second = await handleDaltokiWebhook(webhookRequest(raw), env);
	assert.equal(first.status, 202);
	assert.equal(second.status, 202);
	const page = await pull(env);
	assert.equal(page.items.length, 1);
});

test("same delivery id with different bytes returns 409", async () => {
	const { env } = createHarness();
	const first = await handleDaltokiWebhook(
		webhookRequest(Buffer.from('{"action":"opened"}')),
		env,
	);
	assert.equal(first.status, 202);
	const conflict = await handleDaltokiWebhook(
		webhookRequest(Buffer.from('{"action":"closed"}')),
		env,
	);
	assert.equal(conflict.status, 409);
	assert.equal(await conflict.text(), "daltoki: delivery id conflict");
});

test("invalid webhook signature returns 401", async () => {
	const { env } = createHarness();
	const response = await handleDaltokiWebhook(
		webhookRequest(Buffer.from("{}"), { secret: "wrong-secret" }),
		env,
	);
	assert.equal(response.status, 401);
	assert.equal(await response.text(), "daltoki: invalid webhook signature");
});

test("oversize webhook payload returns 413", async () => {
	const { env } = createHarness();
	const raw = Buffer.alloc(1048576 + 1, 7);
	const response = await handleDaltokiWebhook(webhookRequest(raw), env);
	assert.equal(response.status, 413);
	assert.equal(await response.text(), "daltoki: webhook exceeds 1048576 bytes");
	assert.equal((await pull(env)).items.length, 0);
});

test("missing delivery headers return 400", async () => {
	const { env } = createHarness();
	const request = new Request("https://example/daltoki/webhook", {
		method: "POST",
		body: "{}",
	});
	const response = await handleDaltokiWebhook(request, env);
	assert.equal(response.status, 400);
});

test("relay routes reject a missing or wrong bearer token", async () => {
	const { env } = createHarness();
	const missing = await handleDaltokiRelay(
		new Request("https://example/daltoki/relay/pull", {
			method: "POST",
			body: "{}",
		}),
		env,
	);
	assert.equal(missing.status, 401);
	const wrong = await handleDaltokiRelay(
		relayRequest("/daltoki/relay/pull", { after: "" }, "wrong"),
		env,
	);
	assert.equal(wrong.status, 401);
	const ackWrong = await handleDaltokiRelay(
		relayRequest("/daltoki/relay/ack", { through: "delivery-1" }, "wrong"),
		env,
	);
	assert.equal(ackWrong.status, 401);
});

test("ack after durability removes only acknowledged envelopes", async () => {
	const { env } = createHarness();
	for (const id of ["delivery-1", "delivery-2"]) {
		const response = await handleDaltokiWebhook(
			webhookRequest(Buffer.from(`{"id":"${id}"}`), { delivery: id }),
			env,
		);
		assert.equal(response.status, 202);
	}
	const ack = await handleDaltokiRelay(
		relayRequest("/daltoki/relay/ack", { through: "delivery-1" }),
		env,
	);
	assert.equal(ack.status, 204);
	const page = await pull(env, "delivery-1");
	assert.equal(page.items.length, 1);
	assert.equal((page.items[0] as RelayEnvelope).deliveryID, "delivery-2");
	const drained = await pull(env, "delivery-2");
	assert.equal(drained.items.length, 0);
	assert.equal(drained.next, "delivery-2");
});

test("tombstone keeps deduplicating after ack", async () => {
	const { env } = createHarness();
	const raw = Buffer.from(JSON.stringify({ action: "opened" }));
	assert.equal(
		(await handleDaltokiWebhook(webhookRequest(raw), env)).status,
		202,
	);
	assert.equal(
		(
			await handleDaltokiRelay(
				relayRequest("/daltoki/relay/ack", { through: "delivery-1" }),
				env,
			)
		).status,
		204,
	);
	assert.equal(
		(await handleDaltokiWebhook(webhookRequest(raw), env)).status,
		202,
	);
	assert.equal((await pull(env)).items.length, 0);
});
