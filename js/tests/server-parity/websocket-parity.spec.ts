import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startHost } from "./host.ts";

for (const scenario of ["core", "echo"]) {
  describe(`${scenario} WebSocket socket parity`, () => {
    let hosts: Awaited<ReturnType<typeof startHost>>[] = [];
    beforeAll(async () => { for (const implementation of ["upstream", "rust"] as const) hosts.push(await startHost(scenario, implementation)); });
    afterAll(async () => { await Promise.all(hosts.map((host) => host.stop())); });
    it.each(["text", "binary"])("preserves %s messages and close code", async (kind) => {
      const outputs = await Promise.all(hosts.map(async (host) => {
        const socket = new WebSocket(host.url.replace("http:", "ws:") + (scenario === "core" ? "/ws" : ""), ["chat", "fallback"]);
        socket.binaryType = "arraybuffer";
        await new Promise<void>((resolve, reject) => { socket.onopen = () => resolve(); socket.onerror = reject; });
        const received = new Promise<MessageEvent>((resolve, reject) => { socket.onmessage = resolve; socket.onerror = reject; });
        socket.send(kind === "text" ? "hello 世界" : new Uint8Array([0, 1, 255]));
        const message = (await received).data;
        const closed = new Promise<CloseEvent>((resolve) => { socket.onclose = resolve; });
        socket.close(3001, "finished"); const event = await closed;
        return { message: typeof message === "string" ? message : Array.from(new Uint8Array(message)), type: typeof message, protocol: socket.protocol, code: event.code, reason: event.reason };
      }));
      expect(outputs[1]).toEqual(outputs[0]);
    });
    if (scenario === "echo") it("streams the HTTP body with its content type", async () => {
      const outputs = await Promise.all(hosts.map(async (host) => {
        const response = await fetch(host.url, { method: "POST", headers: { "content-type": "application/custom" }, body: new Uint8Array([0, 1, 255, 2]) });
        return [response.status, response.headers.get("content-type"), Array.from(new Uint8Array(await response.arrayBuffer()))];
      }));
      expect(outputs[1]).toEqual(outputs[0]);
    });
  });
}
