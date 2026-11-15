import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startHost } from "./host.ts";

describe("Cordis and Rust HTTP socket parity", () => {
  let hosts: Awaited<ReturnType<typeof startHost>>[] = [];
  beforeAll(async () => {
    for (const implementation of ["upstream", "rust"] as const) hosts.push(await startHost("core", implementation));
  });
  afterAll(async () => { await Promise.all(hosts.map((host) => host.stop())); });

  it.each([
    ["/hello", "GET"], ["/hello", "HEAD"], ["/hello", "OPTIONS"],
    ["/missing", "GET"], ["/params/a+b", "GET"], ["/params/a%2Fb", "GET"],
    ["/params/%ZZ", "GET"], ["/HELLO/", "GET"], ["/status", "GET"],
    ["/error", "GET"], ["/claimed-error", "GET"], ["/next", "GET"],
    ["/allow", "OPTIONS"], ["/api/scoped", "GET"], ["/scoped", "GET"],
    ["/query?a=1&a=2&b=a+b", "GET"], ["/stream", "GET"], ["/body", "POST"],
  ])("%s %s", async (path, method) => {
    const outputs = await Promise.all(hosts.map(async (host) => {
      const response = await fetch(host.url + path, { method, body: method === "POST" ? "a real body\u0000末尾" : undefined });
      return {
        status: response.status, statusText: response.statusText,
        headers: Object.fromEntries(["allow", "content-type", "location", "x-inspected"].map((name) => [name, response.headers.get(name)])),
        body: Array.from(new Uint8Array(await response.arrayBuffer())),
      };
    }));
    expect(outputs[1]).toEqual(outputs[0]);
  });
});
