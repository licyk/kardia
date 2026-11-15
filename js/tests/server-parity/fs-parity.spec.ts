import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startHost } from "./host.ts";

describe("Cordis and Rust directory base parity", () => {
  const hosts: Awaited<ReturnType<typeof startHost>>[] = [];
  beforeAll(async () => {
    for (const implementation of ["upstream", "rust"] as const) hosts.push(await startHost("fs", implementation));
  });
  afterAll(async () => { await Promise.all(hosts.map(host => host.stop())); });
  const cases = [
    { config: { baseUrl: "file:///app?x#h" }, intercepts: ["data", "logs?x#h"], path: "a.txt?q#f" },
    { config: { baseUrl: "memory:///app" }, intercepts: ["data", "../logs"], path: "../a%20b" },
    { config: { baseUrl: "https://example.com/app" }, intercepts: ["//other.test/data"], path: "a+b" },
    { config: { baseUrl: "memory:///app", unknown: true }, path: "file:///absolute/file" },
    { config: { baseUrl: "memory:///app" }, path: "memory:bucket" },
    { config: { baseUrl: "memory:bucket" }, path: "file" },
    { config: { baseUrl: "relative/path" }, path: "file" },
    { config: { baseUrl: "file:///app" }, intercepts: ["memory:bucket"], path: "file" },
    { config: { baseUrl: "file:///app" }, intercepts: ["?only=query#fragment"], path: "file" },
    { config: { baseUrl: "file:///app" }, intercepts: ["./", "", "deep//path"], path: "file" },
    { config: { baseUrl: "memory:///app" }, path: "C:/data/file" },
  ];
  if (process.platform === "win32") cases.push(
    { config: { baseUrl: "C:\\app space#%" }, intercepts: ["D:\\data"], path: "E:\\file#%.txt" },
    { config: { baseUrl: "C:/app" }, intercepts: ["D:/data"], path: "relative.txt" },
  );
  it.each(cases)("resolves %j", async input => {
    const results = await Promise.all(hosts.map(async host => {
      const response = await fetch(host.url + "/resolve", { method: "POST", body: JSON.stringify(input) });
      expect(response.status).toBe(200);
      return response.json();
    }));
    expect(results[1]).toEqual(results[0]);
  });
});
