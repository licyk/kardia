import { request } from "node:http";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startHost } from "./host.ts";

function probe(url: string, host: string, websocket: boolean) {
  return new Promise<{ status: number | undefined; body: string; allow: string | undefined }>((resolve, reject) => {
    const req = request(url, { headers: { host, ...(websocket ? { connection: "Upgrade", upgrade: "websocket", "sec-websocket-version": "13", "sec-websocket-key": "dGhlIHNhbXBsZSBub25jZQ==" } : {}) } });
    req.on("response", (res) => { let body = ""; res.on("data", (chunk) => { body += chunk; }); res.on("end", () => resolve({ status: res.statusCode, body, allow: res.headers.allow })); res.on("error", reject); });
    req.on("upgrade", (res, socket) => { socket.destroy(); resolve({ status: res.statusCode, body: "", allow: res.headers.allow }); });
    req.on("error", reject); req.end();
  });
}
for (const scenario of ["acl", "acl-list"]) describe(`${scenario} HTTP and WS parity`, () => {
  let hosts: Awaited<ReturnType<typeof startHost>>[] = [];
  beforeAll(async () => { for (const implementation of ["upstream", "rust"] as const) hosts.push(await startHost(scenario, implementation)); });
  afterAll(async () => { await Promise.all(hosts.map((host) => host.stop())); });
  it.each([
    ["/default", "localhost:3000"], ["/default", "[::1]:8443"], ["/default", "127.0.0.1"],
    ["/default", "evil.com"], ["/default", "EXAMPLE.COM:8443"], ["/default", "a.b.example.com"],
    ["/default", "notexample.com"], ["/public", "evil.com"], ["/local", "example.com"],
    ["/list", "partner.com"], ["/list", "example.com"], ["/fallthrough", "evil.com"], ["/fallthrough", "localhost"],
  ])("%s Host %s", async (path, host) => {
    for (const websocket of [false, true]) {
      const outputs = await Promise.all(hosts.map((target) => probe(target.url + path, host, websocket)));
      expect(outputs[1]).toEqual(outputs[0]);
    }
  });
});
