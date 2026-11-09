import { Container } from "@cloudflare/containers";

type Env = {
	BOX: DurableObjectNamespace;
	DALGONA_TOKEN: string;
};

const encoder = new TextEncoder();

function sameSecret(left: string, right: string): boolean {
	const a = encoder.encode(left);
	const b = encoder.encode(right);
	let difference = a.length ^ b.length;
	const length = Math.max(a.length, b.length);
	for (let i = 0; i < length; i += 1) {
		difference |= (a[i] ?? 0) ^ (b[i] ?? 0);
	}
	return difference === 0;
}

function authorized(request: Request, token: string): boolean {
	const header = request.headers.get("Authorization") ?? "";
	const prefix = "Bearer ";
	if (!header.startsWith(prefix)) {
		return false;
	}
	return sameSecret(header.slice(prefix.length), token);
}

function routedRequest(request: Request, path: "/" | "/bridge"): Request {
	const source = new URL(request.url);
	source.pathname = path;
	return new Request(source, request);
}

export class Box extends Container<Env> {
	override defaultPort = 7438;
	override sleepAfter = "6h";

	constructor(ctx: DurableObjectState<Env>, env: Env) {
		super(ctx, env);
		this.envVars = { DALGONA_BOX_TOKEN: env.DALGONA_TOKEN };
	}

	override async fetch(request: Request): Promise<Response> {
		const lifecycle = request.headers.get("X-Dalgona-Lifecycle");
		if (lifecycle === "start") {
			await this.startAndWaitForPorts({ ports: 7438 });
			return new Response(null, { status: 204 });
		}
		if (lifecycle === "destroy") {
			await this.destroy();
			return new Response(null, { status: 204 });
		}
		return super.fetch(request);
	}
}

type Target = { box: DurableObjectStub; path: "/" | "/bridge" };

const BOX_NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/;

function boxFor(request: Request, env: Env): Target | null {
	const url = new URL(request.url);
	const match = /^\/box\/([^/]+)(\/bridge)?\/?$/.exec(url.pathname);
	if (match === null || match[1] === undefined) {
		return null;
	}
	if (!BOX_NAME_PATTERN.test(match[1])) {
		return null;
	}
	const path = match[2] === "/bridge" ? "/bridge" : "/";
	return { box: env.BOX.getByName(match[1]), path };
}

async function handle(request: Request, env: Env): Promise<Response> {
	if (!authorized(request, env.DALGONA_TOKEN)) {
		return new Response("unauthorized", { status: 401 });
	}
	const target = boxFor(request, env);
	if (target === null) {
		return new Response("not found", { status: 404 });
	}
	if (
		(request.method === "POST" || request.method === "DELETE") &&
		target.path !== "/"
	) {
		return new Response("not found", { status: 404 });
	}
	if (request.method === "POST") {
		const lifecycle = new Request(request, {
			headers: { "X-Dalgona-Lifecycle": "start" },
		});
		return target.box.fetch(lifecycle);
	}
	if (request.method === "DELETE") {
		const lifecycle = new Request(request, {
			headers: { "X-Dalgona-Lifecycle": "destroy" },
		});
		return target.box.fetch(lifecycle);
	}
	if (
		request.method === "GET" &&
		request.headers.get("Upgrade")?.toLowerCase() === "websocket"
	) {
		return target.box.fetch(routedRequest(request, target.path));
	}
	return new Response("method not allowed", { status: 405 });
}

export default { fetch: handle };
