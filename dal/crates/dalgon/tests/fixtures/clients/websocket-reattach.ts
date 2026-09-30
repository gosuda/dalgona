type Position = { gen: number; seq: number };

type RpcMessage = {
  id?: number;
  method?: string;
  result?: unknown;
  params?: Record<string, unknown>;
};

export class ReattachingClient {
  private socket?: WebSocket;
  private nextId = 1;
  private position?: Position;

  constructor(
    private readonly url: string,
    private readonly token: string,
    private readonly sessionId: string,
  ) {}

  async connect(): Promise<void> {
    const socket = new WebSocket(this.url, ["dal.v1", `dal.bearer.${this.token}`]);
    this.socket = socket;
    await new Promise<void>((resolve, reject) => {
      socket.addEventListener("open", () => resolve(), { once: true });
      socket.addEventListener("error", () => reject(new Error("WebSocket connection failed")), { once: true });
    });
    await this.request("initialize", { protocol: 1, capabilities: ["sessions"] });
    await this.subscribe();
  }

  async subscribe(): Promise<void> {
    const after = this.position ?? { gen: 0, seq: 0 };
    await this.request("session/subscribe", {
      session: this.sessionId,
      after: { gen: after.gen, seq: after.seq },
    });
  }

  async receive(): Promise<RpcMessage> {
    const socket = this.socket;
    if (!socket) throw new Error("WebSocket is not connected");
    return await new Promise<RpcMessage>((resolve, reject) => {
      socket.addEventListener("message", event => {
        if (typeof event.data !== "string") {
          reject(new Error("expected one JSON-RPC message per text frame"));
          return;
        }
        resolve(JSON.parse(event.data) as RpcMessage);
      }, { once: true });
      socket.addEventListener("error", () => reject(new Error("WebSocket receive failed")), { once: true });
    });
  }

  async remember(message: RpcMessage): Promise<void> {
    if (message.method !== "session/update" || !message.params) return;
    const params = message.params;
    if (params.type === "resync") {
      const view = await this.request("session/view", { session: this.sessionId, page: {} });
      const pair = view as Position;
      this.position = { gen: pair.gen, seq: pair.seq };
      return;
    }
    const update = params.update as Record<string, unknown> | undefined;
    const gen = params.gen;
    const seq = params.seq;
    if (typeof gen === "number" && typeof seq === "number") this.position = { gen, seq };
    if (!update) return;
  }

  disconnect(): void {
    this.socket?.close();
    this.socket = undefined;
  }

  private async request(method: string, params: Record<string, unknown>): Promise<unknown> {
    const socket = this.socket;
    if (!socket) throw new Error("WebSocket is not connected");
    const id = this.nextId++;
    socket.send(JSON.stringify({ id, method, params }));
    for (;;) {
      const message = await this.receive();
      if (message.id === id) return message.result;
      await this.remember(message);
    }
  }
}
