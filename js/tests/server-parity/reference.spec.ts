import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startHost } from "./host.ts";

describe("fixed Cordis reference over real sockets", () => {
  let host: Awaited<ReturnType<typeof startHost>>;
  beforeAll(async () => { host = await startHost("core"); });
  afterAll(async () => { await host?.stop(); });

  it.each([
    ["/hello", "GET", 200, "world", null],
    ["/hello", "HEAD", 405, "", "get"],
    ["/hello", "OPTIONS", 204, "", "get"],
    ["/missing", "GET", 404, "", null],
    ["/params/a+b", "GET", 200, "a+b", null],
    ["/params/a%2Fb", "GET", 200, "a/b", null],
    ["/HELLO/", "GET", 200, "world", null],
    ["/status", "GET", 204, "", null],
    ["/error", "GET", 500, "", null],
    ["/claimed-error", "GET", 503, "", null],
    ["/next", "GET", 200, '{"ok":true}', null],
    ["/allow", "OPTIONS", 204, "", "*"],
    ["/api/scoped", "GET", 200, "scoped", null],
    ["/scoped", "GET", 404, "", null],
  ])("%s %s", async (path, method, status, body, allow) => {
    const response = await fetch(host.url + path, { method: method as string });
    expect(response.status).toBe(status);
    expect(await response.text()).toBe(body);
    expect(response.headers.get("allow")).toBe(allow);
  });
  it("keeps the stream after two concurrent inspections", async () => {
    const response = await fetch(host.url + "/stream");
    expect(response.headers.get("x-inspected")).toBe("hello world|hello world");
    expect(await response.text()).toBe("hello world");
  });
  it("preserves repeated query pairs", async () => {
    expect(await (await fetch(host.url + "/query?a=1&a=2&b=a+b")).json()).toEqual([["a", "1"], ["a", "2"], ["b", "a b"]]);
  });
});

for (const scenario of ["echo", "proxy"]) {
  describe(`${scenario} reference`, () => {
    let host: Awaited<ReturnType<typeof startHost>>;
    beforeAll(async () => { host = await startHost(scenario); });
    afterAll(async () => { await host?.stop(); });
    it("echoes a real HTTP body", async () => {
      const response = await fetch(host.url, { method: "POST", headers: { "content-type": "text/custom" }, body: "hello" });
      expect(response.status).toBe(200);
      expect(response.headers.get("content-type")).toBe("text/custom");
      expect(await response.text()).toBe("hello");
    });
    it.each(["text", "binary"])("echoes a real WebSocket %s message", async (kind) => {
      const socket = new WebSocket(host.url.replace("http:", "ws:"));
      socket.binaryType = "arraybuffer";
      try {
        await new Promise<void>((resolve, reject) => { socket.onopen = () => resolve(); socket.onerror = reject; });
        const received = new Promise<MessageEvent>((resolve, reject) => { socket.onmessage = resolve; socket.onerror = reject; });
        const data = kind === "text" ? "hello" : new Uint8Array([0, 1, 255]);
        socket.send(data);
        const event = await received;
        if (kind === "text") expect(event.data).toBe(data);
        else expect(new Uint8Array(event.data)).toEqual(data);
      } finally {
        const closed = new Promise<void>((resolve) => { socket.onclose = () => resolve(); });
        socket.close(1000); await closed;
      }
    });
  });
}
