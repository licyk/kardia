import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { createRequire } from "node:module";
import { join } from "node:path";
import { upstream } from "./upstream.ts";
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
    ["/optional", "GET"], ["/optional/42", "GET"], ["/wild/a/b/c", "GET"],
    ["/mix/a-b-c", "GET"], ["/wildext/a/b.tar.gz", "GET"], ["/two/a/b/c/d", "GET"],
    ["/quoted/a+b", "GET"], ["/unicode/%E4%B8%AD%E6%96%87", "GET"], ["/regex/123.json", "GET"],
    ["/raw", "GET"], ["/reason", "GET"], ["/blob", "GET"], ["/consumption", "POST"],
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

  it.each([
    "*/*", "text/html, application/json;q=0.8", "application/json, text/html;q=0.5",
    "text/*;q=0.9, text/html;q=0, */*;q=0.8", "application/xml", "",
    "text/html;level=1;q=0.4, text/html;q=0.8", 'text/html;note="a,b";q=0.2, application/json;q=0.7',
  ])("negotiates Accept %s", async (accept) => {
    const outputs = await Promise.all(hosts.map(async (host) => (await fetch(host.url + "/accept?type=html&type=json&type=text/plain", { headers: { accept } })).json()));
    expect(outputs[1]).toEqual(outputs[0]);
  });
  it("parses URL-encoded duplicate fields", async () => {
    const outputs = await Promise.all(hosts.map(async (host) => (await fetch(host.url + "/form", { method: "POST", body: new URLSearchParams("a=1&a=2&name=%E4%B8%AD%E6%96%87+text") })).json()));
    expect(outputs[1]).toEqual(outputs[0]);
  });
  it("parses real multipart text and binary files", async () => {
    const form = new FormData(); form.append("a", "one"); form.append("a", "two\nlines"); form.append("file", new Blob([new Uint8Array([0, 1, 255])], { type: "application/octet-stream" }), "测试.bin");
    const outputs = await Promise.all(hosts.map(async (host) => (await fetch(host.url + "/form", { method: "POST", body: form })).json()));
    expect(outputs[1]).toEqual(outputs[0]);
  });
  it("handles malformed JSON as an unclaimed server error", async () => {
    const outputs = await Promise.all(hosts.map(async (host) => { const res = await fetch(host.url + "/json", { method: "POST", body: "{" }); return [res.status, await res.text()]; }));
    expect(outputs[1]).toEqual(outputs[0]);
  });
  it.each(['g','y','gy','i','iu'])('ECMAScript regexp flags %s',async flags=>{
    const path=flags.includes('i')?'/fold/'+flags+'/K':'/state/'+flags+'/123';
    const results=[];
    for(const host of hosts){const output=[];for(let i=0;i<6;i++){const res=await fetch(host.url+path);output.push([res.status,await res.text()]);}results.push(output);}
    expect(results[1]).toEqual(results[0]);
  });
  it('negotiates every fixed upstream MIME extension',async()=>{
    const mime=createRequire(join(upstream,'package.json'))('mime-types');
    const entries=Object.entries(mime.types) as [string,string][];
    for(let offset=0;offset<entries.length;offset+=20){await Promise.all(entries.slice(offset,offset+20).map(async([extension,type])=>{
      const results=await Promise.all(hosts.map(async host=>(await fetch(host.url+'/accept?type='+encodeURIComponent(extension),{headers:{accept:type}})).json()));
      expect(results[1],extension).toEqual(results[0]);
    }));}
  });
  it.each(['[1.0,-0.0,1e-7,1e21,9007199254740993,1e400]','{"2":"b","1":"a","x":1}'])('JSON number and property encoding %s',async body=>{
    const results=await Promise.all(hosts.map(async host=>{const response=await fetch(host.url+'/json',{method:'POST',body});return [response.status,await response.text()];}));expect(results[1]).toEqual(results[0]);
  });
});
