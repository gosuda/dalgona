import { strict as assert } from "node:assert";
import { test } from "node:test";
import worker, { Box, boxToken } from "./index.ts";

type SeenRequest = {
	authorization: string | null;
	lifecycle: string | null;
	method: string;
	pathname: string;
};

type Stub = {
	fetch: (request: Request) => Promise<Response>;
	seen: SeenRequest[];
};

type WorkerEnv = Parameters<typeof worker.fetch>[1];

type FakeEnv = {
	BOX: { getByName: (name: string) => Stub };
	DALGONA_TOKEN: string;
};

const MASTER_TOKEN = "test-master-token";

function createStub(): Stub {
	const seen: SeenRequest[] = [];
	const fetch = async (request: Request): Promise<Response> => {
		seen.push({
			authorization: request.headers.get("Authorization"),
			lifecycle: request.headers.get("X-Dalgona-Lifecycle"),
			method: request.method,
			pathname: new URL(request.url).pathname,
		});
		return new Response("stub", { status: 200 });
	};
	return { fetch, seen };
}

function workerEnvFor(fake: FakeEnv): WorkerEnv {
	if (typeof fake.BOX.getByName !== "function") {
		throw new Error("test setup: fake BOX binding lacks getByName");
	}
	if (typeof fake.DALGONA_TOKEN !== "string") {
		throw new Error("test setup: fake env lacks DALGONA_TOKEN");
	}
	const validated: unknown = fake;
	return validated as WorkerEnv;
}

function createEnv(): { env: WorkerEnv; stubs: Map<string, Stub> } {
	const stubs = new Map<string, Stub>();
	const getByName = (name: string): Stub => {
		const existing = stubs.get(name);
		if (existing !== undefined) {
			return existing;
		}
		const stub = createStub();
		stubs.set(name, stub);
		return stub;
	};
	const env = workerEnvFor({ BOX: { getByName }, DALGONA_TOKEN: MASTER_TOKEN });
	return { env, stubs };
}

function request(
	path: string,
	init?: {
		bearer?: string;
		headers?: Record<string, string>;
		method?: string;
	},
): Request {
	const base: Record<string, string> = { ...(init?.headers ?? {}) };
	const headers =
		init?.bearer === undefined
			? base
			: { ...base, Authorization: `Bearer ${init.bearer}` };
	return new Request(`https://worker.test${path}`, {
		headers,
		method: init?.method ?? "GET",
	});
}

function stubFor(stubs: Map<string, Stub>, name: string): Stub {
	const stub = stubs.get(name);
	if (stub === undefined) {
		throw new Error(`test setup: no stub recorded for box ${name}`);
	}
	return stub;
}

function firstSeen(stub: Stub): SeenRequest {
	const seen = stub.seen.at(0);
	if (seen === undefined) {
		throw new Error("test setup: stub recorded no request");
	}
	return seen;
}

test("accepts a valid box name", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/my-box_1", { bearer: MASTER_TOKEN, method: "POST" }),
		env,
	);
	assert.equal(response.status, 200);
	assert.equal(stubFor(stubs, "my-box_1").seen.length, 1);
});

test("accepts a 64 character box name", async () => {
	const { env, stubs } = createEnv();
	const name = "a".repeat(64);
	const response = await worker.fetch(
		request(`/box/${name}`, { bearer: MASTER_TOKEN, method: "POST" }),
		env,
	);
	assert.equal(response.status, 200);
	assert.equal(stubFor(stubs, name).seen.length, 1);
});

test("rejects a 65 character box name with 400", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request(`/box/${"a".repeat(65)}`, { bearer: MASTER_TOKEN, method: "POST" }),
		env,
	);
	assert.equal(response.status, 400);
	assert.equal(stubs.size, 0);
});

test("rejects a box name with a dot with 400", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/has.dot", { bearer: MASTER_TOKEN, method: "POST" }),
		env,
	);
	assert.equal(response.status, 400);
	assert.equal(stubs.size, 0);
});

test("rejects a box name with a space with 400", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/has%20space", { bearer: MASTER_TOKEN, method: "POST" }),
		env,
	);
	assert.equal(response.status, 400);
	assert.equal(stubs.size, 0);
});

test("rejects a box name starting with a dash with 400", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/-lead", { bearer: MASTER_TOKEN, method: "POST" }),
		env,
	);
	assert.equal(response.status, 400);
	assert.equal(stubs.size, 0);
});

test("treats a slash after the box name as an unknown route with 404", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha/extra", { bearer: MASTER_TOKEN }),
		env,
	);
	assert.equal(response.status, 404);
	assert.equal(stubs.size, 0);
});

test("treats an empty box name as an unknown route with 404", async () => {
	const { env } = createEnv();
	const response = await worker.fetch(
		request("/box/", { bearer: MASTER_TOKEN }),
		env,
	);
	assert.equal(response.status, 404);
});

test("returns 404 for an unknown route", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/nothing", { bearer: MASTER_TOKEN }),
		env,
	);
	assert.equal(response.status, 404);
	assert.equal(stubs.size, 0);
});

test("returns 404 for POST on the bridge route", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha/bridge", { bearer: MASTER_TOKEN, method: "POST" }),
		env,
	);
	assert.equal(response.status, 404);
	assert.equal(stubFor(stubs, "alpha").seen.length, 0);
});

test("returns 404 for DELETE on the bridge route", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha/bridge", { bearer: MASTER_TOKEN, method: "DELETE" }),
		env,
	);
	assert.equal(response.status, 404);
	assert.equal(stubFor(stubs, "alpha").seen.length, 0);
});

test("returns 405 for PUT on a box route", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha", { bearer: MASTER_TOKEN, method: "PUT" }),
		env,
	);
	assert.equal(response.status, 405);
	assert.equal(stubFor(stubs, "alpha").seen.length, 0);
});

test("returns 405 for a plain GET on a box route", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha", { bearer: MASTER_TOKEN }),
		env,
	);
	assert.equal(response.status, 405);
	assert.equal(stubFor(stubs, "alpha").seen.length, 0);
});

test("proxies a websocket GET to the box root", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha", {
			bearer: MASTER_TOKEN,
			headers: { Upgrade: "websocket" },
		}),
		env,
	);
	assert.equal(response.status, 200);
	assert.equal(firstSeen(stubFor(stubs, "alpha")).pathname, "/");
});

test("proxies a websocket GET to the bridge path", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha/bridge", {
			bearer: MASTER_TOKEN,
			headers: { Upgrade: "websocket" },
		}),
		env,
	);
	assert.equal(response.status, 200);
	assert.equal(firstSeen(stubFor(stubs, "alpha")).pathname, "/bridge");
});

test("starts the box container on POST", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha", { bearer: MASTER_TOKEN, method: "POST" }),
		env,
	);
	assert.equal(response.status, 200);
	assert.equal(firstSeen(stubFor(stubs, "alpha")).lifecycle, "start");
});

test("destroys the box container on DELETE", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha", { bearer: MASTER_TOKEN, method: "DELETE" }),
		env,
	);
	assert.equal(response.status, 200);
	assert.equal(firstSeen(stubFor(stubs, "alpha")).lifecycle, "destroy");
});

test("derives the same token for the same box", async () => {
	const first = await boxToken(MASTER_TOKEN, "alpha");
	const second = await boxToken(MASTER_TOKEN, "alpha");
	assert.equal(first, second);
});

test("derives different tokens for different boxes", async () => {
	const alpha = await boxToken(MASTER_TOKEN, "alpha");
	const beta = await boxToken(MASTER_TOKEN, "beta");
	assert.notEqual(alpha, beta);
});

test("derives a box token that differs from the master token", async () => {
	const token = await boxToken(MASTER_TOKEN, "alpha");
	assert.notEqual(token, MASTER_TOKEN);
	assert.match(token, /^[0-9a-f]{64}$/);
});

test("rejects the token for one box on another box route", async () => {
	const { env, stubs } = createEnv();
	const token = await boxToken(MASTER_TOKEN, "alpha");
	const response = await worker.fetch(
		request("/box/beta", { bearer: token, method: "POST" }),
		env,
	);
	assert.equal(response.status, 401);
	assert.equal(stubs.size, 0);
});

test("rejects a box token on its own box route", async () => {
	const { env, stubs } = createEnv();
	const token = await boxToken(MASTER_TOKEN, "alpha");
	const response = await worker.fetch(
		request("/box/alpha", {
			bearer: token,
			headers: { Upgrade: "websocket" },
		}),
		env,
	);
	assert.equal(response.status, 401);
	assert.equal(stubs.size, 0);
});

test("returns 401 without an authorization header", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(request("/box/alpha"), env);
	assert.equal(response.status, 401);
	assert.equal(stubs.size, 0);
});

test("returns 401 for a wrong bearer", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha", { bearer: "wrong-token" }),
		env,
	);
	assert.equal(response.status, 401);
	assert.equal(stubs.size, 0);
});

test("returns 401 for a non bearer scheme", async () => {
	const { env, stubs } = createEnv();
	const response = await worker.fetch(
		request("/box/alpha", { headers: { Authorization: "Basic abc" } }),
		env,
	);
	assert.equal(response.status, 401);
	assert.equal(stubs.size, 0);
});

type BoxContext = ConstructorParameters<typeof Box>[0];
type BoxEnvironment = ConstructorParameters<typeof Box>[1];

type ObservedBox = {
	ctx: { id: { name: string | undefined } };
	envVars: Record<string, string> | undefined;
	forwarded: Request[];
};

function observed(box: Box): ObservedBox {
	const candidate: unknown = box;
	return candidate as ObservedBox;
}

function boxContext(name: string | undefined): BoxContext {
	const ctx: unknown = { id: { name } };
	return ctx as BoxContext;
}

function boxEnvironment(): BoxEnvironment {
	const env: unknown = {
		BOX: {
			getByName: (): never => {
				throw new Error("test setup: Box test never uses BOX");
			},
		},
		DALGONA_TOKEN: MASTER_TOKEN,
	};
	return env as BoxEnvironment;
}

function createBox(name: string | undefined): { box: Box; seen: ObservedBox } {
	const box = new Box(boxContext(name), boxEnvironment());
	return { box, seen: observed(box) };
}

test("Box answers 500 and forwards nothing without a box name", async () => {
	const missing = createBox(undefined);
	const missingResponse = await missing.box.fetch(
		new Request("https://worker.test/"),
	);
	assert.equal(missingResponse.status, 500);
	assert.equal(missing.seen.forwarded.length, 0);

	const empty = createBox("");
	const emptyResponse = await empty.box.fetch(
		new Request("https://worker.test/"),
	);
	assert.equal(emptyResponse.status, 500);
	assert.equal(empty.seen.forwarded.length, 0);
});

test("Box forwards the derived box token instead of the worker token", async () => {
	const { box, seen } = createBox("alpha");
	const expected = await boxToken(MASTER_TOKEN, "alpha");
	const response = await box.fetch(
		new Request("https://worker.test/", {
			headers: { Authorization: `Bearer ${MASTER_TOKEN}` },
		}),
	);
	assert.equal(response.status, 200);
	assert.equal(seen.forwarded.length, 1);
	const forwarded = seen.forwarded[0];
	if (forwarded === undefined) {
		throw new Error("test setup: container received no request");
	}
	assert.equal(forwarded.headers.get("Authorization"), `Bearer ${expected}`);
	assert.notEqual(
		forwarded.headers.get("Authorization"),
		`Bearer ${MASTER_TOKEN}`,
	);
});

test("Box exposes the derived token as DALGONA_BOX_TOKEN", async () => {
	const { box, seen } = createBox("alpha");
	const expected = await boxToken(MASTER_TOKEN, "alpha");
	const response = await box.fetch(new Request("https://worker.test/"));
	assert.equal(response.status, 200);
	assert.equal(seen.envVars?.["DALGONA_BOX_TOKEN"], expected);
	assert.notEqual(seen.envVars?.["DALGONA_BOX_TOKEN"], MASTER_TOKEN);
});

test("Boxes for different names forward different tokens", async () => {
	const first = createBox("alpha");
	const second = createBox("beta");
	const firstResponse = await first.box.fetch(
		new Request("https://worker.test/"),
	);
	const secondResponse = await second.box.fetch(
		new Request("https://worker.test/"),
	);
	assert.equal(firstResponse.status, 200);
	assert.equal(secondResponse.status, 200);
	const firstForwarded = first.seen.forwarded[0];
	const secondForwarded = second.seen.forwarded[0];
	if (firstForwarded === undefined || secondForwarded === undefined) {
		throw new Error("test setup: container received no request");
	}
	const firstAuth = firstForwarded.headers.get("Authorization");
	const secondAuth = secondForwarded.headers.get("Authorization");
	assert.equal(firstAuth, `Bearer ${await boxToken(MASTER_TOKEN, "alpha")}`);
	assert.equal(secondAuth, `Bearer ${await boxToken(MASTER_TOKEN, "beta")}`);
	assert.notEqual(firstAuth, secondAuth);
});
