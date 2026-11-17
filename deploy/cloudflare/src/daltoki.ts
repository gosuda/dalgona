export type DaltokiEnv = {
	DALTOKI_RELAY: DurableObjectNamespace;
	DALTOKI_WEBHOOK_SECRET?: string;
	DALTOKI_RELAY_TOKEN?: string;
};

export type RelayEnvelope = {
	deliveryID: string;
	event: string;
	receivedAt: string;
	bodySHA256: string;
	signature: string;
	body: string;
};

type EnvelopeHead = {
	deliveryID: string;
	event: string;
	receivedAt: string;
	bodySHA256: string;
	signature: string;
	chunks: number;
};

type StoredDelivery = {
	seq: number;
	digest: string;
	receivedAt: string;
	tombstoneUntil: string;
};

type PutOutcome = "stored" | "duplicate" | "conflict" | "full";

export const MAX_WEBHOOK_BYTES = 1048576;
export const MAX_QUEUE_ITEMS = 10000;
export const MAX_QUEUE_BYTES = 64 * 1024 * 1024;
const RETENTION_MS = 7 * 24 * 3600 * 1000;
const TOMBSTONE_MS = 30 * 24 * 3600 * 1000;
const VALUE_CHUNK_BYTES = 96 * 1024;
const MAX_PULL_BODY_BYTES = 768 * 1024;
const LIST_PAGE = 1000;

const textEncoder = new TextEncoder();

export type DaltokiStorage = {
	get<T = unknown>(key: string): Promise<T | undefined>;
	put(key: string, value: unknown): Promise<void>;
	delete(key: string): Promise<boolean>;
	list<T = unknown>(options?: {
		prefix?: string;
		startAfter?: string;
		limit?: number;
	}): Promise<Map<string, T>>;
};

export type DaltokiContext = {
	storage: DaltokiStorage;
};

function hexOf(bytes: ArrayBuffer): string {
	return [...new Uint8Array(bytes)]
		.map((b) => b.toString(16).padStart(2, "0"))
		.join("");
}

function sameBytes(left: string, right: string): boolean {
	const a = textEncoder.encode(left);
	const b = textEncoder.encode(right);
	let difference = a.length ^ b.length;
	const length = Math.max(a.length, b.length);
	for (let i = 0; i < length; i += 1) {
		difference |= (a[i] ?? 0) ^ (b[i] ?? 0);
	}
	return difference === 0;
}

export async function hmacHex(
	secret: string,
	body: ArrayBuffer,
): Promise<string> {
	const key = await crypto.subtle.importKey(
		"raw",
		textEncoder.encode(secret),
		{ name: "HMAC", hash: "SHA-256" },
		false,
		["sign"],
	);
	return hexOf(await crypto.subtle.sign("HMAC", key, body));
}

export async function sha256Hex(body: ArrayBuffer): Promise<string> {
	return hexOf(await crypto.subtle.digest("SHA-256", body));
}

function toBase64(body: ArrayBuffer): string {
	const bytes = new Uint8Array(body);
	let text = "";
	for (const b of bytes) {
		text += String.fromCharCode(b);
	}
	return btoa(text);
}

function fromBase64(body: string): ArrayBuffer {
	const text = atob(body);
	const bytes = new Uint8Array(text.length);
	for (let i = 0; i < text.length; i += 1) {
		bytes[i] = text.charCodeAt(i);
	}
	return bytes.buffer;
}

export class DaltokiRelay {
	private ctx: DaltokiContext;

	constructor(ctx: DaltokiContext) {
		this.ctx = ctx;
	}

	private get storage(): DaltokiStorage {
		return this.ctx.storage;
	}

	async fetch(request: Request): Promise<Response> {
		const url = new URL(request.url);
		if (url.pathname === "/put" && request.method === "POST") {
			return this.put(await request.json());
		}
		if (url.pathname === "/pull" && request.method === "POST") {
			return this.pull(await request.json());
		}
		if (url.pathname === "/ack" && request.method === "POST") {
			return this.ack(await request.json());
		}
		return new Response("not found", { status: 404 });
	}

	private async counters(): Promise<{
		count: number;
		bytes: number;
		head: number;
	}> {
		const count = (await this.storage.get<number>("meta:count")) ?? 0;
		const bytes = (await this.storage.get<number>("meta:bytes")) ?? 0;
		const head = (await this.storage.get<number>("meta:head")) ?? 0;
		return { count, bytes, head };
	}

	private async put(envelope: RelayEnvelope): Promise<Response> {
		if (
			typeof envelope?.deliveryID !== "string" ||
			envelope.deliveryID === "" ||
			typeof envelope?.event !== "string" ||
			envelope.event === "" ||
			typeof envelope?.bodySHA256 !== "string" ||
			typeof envelope?.body !== "string" ||
			envelope.body === ""
		) {
			return Response.json(
				{ error: "daltoki: malformed relay envelope" },
				{ status: 400 },
			);
		}
		let bodyBytes: ArrayBuffer;
		try {
			bodyBytes = fromBase64(envelope.body);
		} catch {
			return Response.json(
				{ error: "daltoki: malformed relay envelope" },
				{ status: 400 },
			);
		}
		const now = Date.now();
		const existing = await this.storage.get<StoredDelivery>(
			`dlv:${envelope.deliveryID}`,
		);
		if (existing !== undefined && Date.parse(existing.tombstoneUntil) < now) {
			await this.storage.delete(`dlv:${envelope.deliveryID}`);
		} else if (existing !== undefined) {
			if (existing.digest === envelope.bodySHA256.toLowerCase()) {
				return Response.json({ result: "duplicate" satisfies PutOutcome });
			}
			return Response.json(
				{ error: "daltoki: delivery id conflict" },
				{ status: 409 },
			);
		}
		const { count, bytes, head } = await this.counters();
		if (
			count >= MAX_QUEUE_ITEMS ||
			bytes + bodyBytes.byteLength > MAX_QUEUE_BYTES
		) {
			return Response.json(
				{ error: "daltoki: relay queue is full" },
				{ status: 503 },
			);
		}
		const receivedAt = Date.parse(envelope.receivedAt);
		const seq = head + 1;
		const chunks = Math.max(
			1,
			Math.ceil(envelope.body.length / VALUE_CHUNK_BYTES),
		);
		await this.storage.put(`env:${seq}`, {
			deliveryID: envelope.deliveryID,
			event: envelope.event,
			receivedAt: envelope.receivedAt,
			bodySHA256: envelope.bodySHA256,
			signature: envelope.signature,
			chunks,
		} satisfies EnvelopeHead);
		for (let index = 0; index < chunks; index += 1) {
			await this.storage.put(
				`env:${seq}:${index}`,
				envelope.body.slice(
					index * VALUE_CHUNK_BYTES,
					(index + 1) * VALUE_CHUNK_BYTES,
				),
			);
		}
		await this.storage.put(`dlv:${envelope.deliveryID}`, {
			seq,
			digest: envelope.bodySHA256.toLowerCase(),
			receivedAt: envelope.receivedAt,
			tombstoneUntil: new Date(
				(Number.isNaN(receivedAt) ? now : receivedAt) + TOMBSTONE_MS,
			).toISOString(),
		} satisfies StoredDelivery);
		await this.storage.put("meta:count", count + 1);
		await this.storage.put("meta:bytes", bytes + bodyBytes.byteLength);
		await this.storage.put("meta:head", seq);
		return Response.json({ result: "stored" satisfies PutOutcome });
	}

	private async pull(payload: {
		after?: unknown;
		limit?: unknown;
	}): Promise<Response> {
		let limit = 100;
		if (typeof payload?.limit === "number" && Number.isFinite(payload.limit)) {
			limit = Math.min(100, Math.max(1, Math.floor(payload.limit)));
		}
		const after = typeof payload?.after === "string" ? payload.after : "";
		let afterSeq = 0;
		if (after !== "") {
			const record = await this.storage.get<StoredDelivery>(`dlv:${after}`);
			if (record !== undefined) {
				afterSeq = record.seq;
			}
		}
		const ordered = (await this.listAll<EnvelopeHead>("env:"))
			.filter(([key]) => !key.slice("env:".length).includes(":"))
			.map(([key, envelope]) => ({
				seq: Number(key.slice("env:".length).split(":")[0]),
				envelope,
			}))
			.filter((entry) => Number.isInteger(entry.seq) && entry.seq > afterSeq)
			.sort((left, right) => left.seq - right.seq);
		const now = Date.now();
		const items: RelayEnvelope[] = [];
		let next = after;
		let bodyBytes = 0;
		for (const entry of ordered) {
			if (items.length >= limit || bodyBytes >= MAX_PULL_BODY_BYTES) {
				break;
			}
			if (Date.parse(entry.envelope.receivedAt) + RETENTION_MS < now) {
				await this.removeEnvelope(entry.seq, entry.envelope);
				continue;
			}
			const envelope = await this.readEnvelope(entry.seq, entry.envelope);
			bodyBytes += envelope.body.length;
			items.push(envelope);
			next = envelope.deliveryID;
		}
		await this.collectTombstones(now);
		return Response.json({ items, next });
	}

	private async listAll<T>(prefix: string): Promise<[string, T][]> {
		const out: [string, T][] = [];
		let startAfter = "";
		for (;;) {
			const page = await this.storage.list<T>({
				prefix,
				startAfter,
				limit: LIST_PAGE,
			});
			if (page.size === 0) {
				return out;
			}
			for (const [key, value] of page) {
				out.push([key, value]);
				startAfter = key;
			}
			if (page.size < LIST_PAGE) {
				return out;
			}
		}
	}

	private async readEnvelope(
		seq: number,
		head: EnvelopeHead,
	): Promise<RelayEnvelope> {
		let body = "";
		for (let index = 0; index < head.chunks; index += 1) {
			body += (await this.storage.get<string>(`env:${seq}:${index}`)) ?? "";
		}
		return {
			deliveryID: head.deliveryID,
			event: head.event,
			receivedAt: head.receivedAt,
			bodySHA256: head.bodySHA256,
			signature: head.signature,
			body,
		};
	}

	private async removeEnvelope(
		seq: number,
		envelope: EnvelopeHead,
	): Promise<void> {
		let size = 0;
		try {
			const body = await this.readEnvelope(seq, envelope);
			size = fromBase64(body.body).byteLength;
		} catch {
			size = 0;
		}
		await this.storage.delete(`env:${seq}`);
		for (let index = 0; index < envelope.chunks; index += 1) {
			await this.storage.delete(`env:${seq}:${index}`);
		}
		const count = (await this.storage.get<number>("meta:count")) ?? 0;
		const bytes = (await this.storage.get<number>("meta:bytes")) ?? 0;
		await this.storage.put("meta:count", Math.max(0, count - 1));
		await this.storage.put("meta:bytes", Math.max(0, bytes - size));
	}

	private async collectTombstones(now: number): Promise<void> {
		for (const [key, record] of await this.listAll<StoredDelivery>("dlv:")) {
			if (Date.parse(record.tombstoneUntil) < now) {
				await this.storage.delete(key);
			}
		}
	}

	private async ack(payload: { through?: unknown }): Promise<Response> {
		const through = typeof payload?.through === "string" ? payload.through : "";
		if (through === "") {
			return Response.json(
				{ error: "daltoki: malformed relay ack" },
				{ status: 400 },
			);
		}
		const record = await this.storage.get<StoredDelivery>(`dlv:${through}`);
		if (record === undefined) {
			return new Response(null, { status: 204 });
		}
		for (const [key, envelope] of await this.listAll<EnvelopeHead>("env:")) {
			const tail = key.slice("env:".length);
			if (tail.includes(":")) {
				continue;
			}
			const seq = Number(tail);
			if (Number.isInteger(seq) && seq <= record.seq) {
				await this.removeEnvelope(seq, envelope);
			}
		}
		return new Response(null, { status: 204 });
	}
}

export async function handleDaltokiWebhook(
	request: Request,
	env: DaltokiEnv,
): Promise<Response> {
	if (request.method !== "POST") {
		return new Response("method not allowed", { status: 405 });
	}
	if (
		env.DALTOKI_WEBHOOK_SECRET === undefined ||
		env.DALTOKI_WEBHOOK_SECRET === ""
	) {
		return new Response("daltoki: relay is not configured", { status: 500 });
	}
	const deliveryID = request.headers.get("X-GitHub-Delivery") ?? "";
	const event = request.headers.get("X-GitHub-Event") ?? "";
	const signature = request.headers.get("X-Hub-Signature-256") ?? "";
	if (deliveryID === "" || event === "" || signature === "") {
		return new Response("daltoki: malformed webhook request", { status: 400 });
	}
	const announced = Number(request.headers.get("Content-Length") ?? Number.NaN);
	if (Number.isFinite(announced) && announced > MAX_WEBHOOK_BYTES) {
		return new Response("daltoki: webhook exceeds 1048576 bytes", {
			status: 413,
		});
	}
	const raw = await request.arrayBuffer();
	if (raw.byteLength > MAX_WEBHOOK_BYTES) {
		return new Response("daltoki: webhook exceeds 1048576 bytes", {
			status: 413,
		});
	}
	const expected = await hmacHex(env.DALTOKI_WEBHOOK_SECRET, raw);
	if (!sameBytes(expected, signature.replace(/^sha256=/, "").toLowerCase())) {
		return new Response("daltoki: invalid webhook signature", { status: 401 });
	}
	try {
		JSON.parse(new TextDecoder().decode(raw));
	} catch {
		return new Response("daltoki: malformed webhook payload", { status: 400 });
	}
	const envelope: RelayEnvelope = {
		deliveryID,
		event,
		receivedAt: new Date().toISOString(),
		bodySHA256: await sha256Hex(raw),
		signature: expected,
		body: toBase64(raw),
	};
	const relay = env.DALTOKI_RELAY.getByName("daltoki-relay");
	const stored = await relay.fetch(
		new Request("https://relay/put", {
			method: "POST",
			body: JSON.stringify(envelope),
		}),
	);
	if (stored.status === 409) {
		return new Response("daltoki: delivery id conflict", { status: 409 });
	}
	if (stored.status === 503) {
		return new Response("daltoki: relay queue is full", { status: 503 });
	}
	return new Response(null, { status: 202 });
}

function relayUnauthorized(request: Request, env: DaltokiEnv): boolean {
	const token = env.DALTOKI_RELAY_TOKEN ?? "";
	if (token === "") {
		return true;
	}
	const header = request.headers.get("Authorization") ?? "";
	const prefix = "Bearer ";
	if (!header.startsWith(prefix)) {
		return true;
	}
	return !sameBytes(header.slice(prefix.length), token);
}

export async function handleDaltokiRelay(
	request: Request,
	env: DaltokiEnv,
): Promise<Response> {
	if (request.method !== "POST") {
		return new Response("method not allowed", { status: 405 });
	}
	if (relayUnauthorized(request, env)) {
		return new Response("unauthorized", { status: 401 });
	}
	let payload: { after?: unknown; limit?: unknown; through?: unknown };
	try {
		payload = await request.json();
	} catch {
		return new Response("daltoki: malformed relay request", { status: 400 });
	}
	const url = new URL(request.url);
	const relay = env.DALTOKI_RELAY.getByName("daltoki-relay");
	if (url.pathname === "/daltoki/relay/pull") {
		return relay.fetch(
			new Request("https://relay/pull", {
				method: "POST",
				body: JSON.stringify(payload),
			}),
		);
	}
	if (url.pathname === "/daltoki/relay/ack") {
		return relay.fetch(
			new Request("https://relay/ack", {
				method: "POST",
				body: JSON.stringify(payload),
			}),
		);
	}
	return new Response("not found", { status: 404 });
}
