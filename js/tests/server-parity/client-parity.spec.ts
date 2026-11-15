import { afterAll,beforeAll,describe,expect,it } from "vitest";
import { createServer, type IncomingMessage,type ServerResponse } from "node:http";
import { createServer as createSecureServer } from "node:https";
import { mkdtemp,writeFile,rm } from "node:fs/promises";
import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gzipSync,deflateSync,deflateRawSync,brotliCompressSync,zstdCompressSync } from "node:zlib";
import { startHost } from "./host.ts";
import { upstream } from "./upstream.ts";
const require=createRequire(join(upstream,"package.json"));
const {WebSocketServer}=require("ws");

function target(req:IncomingMessage,res:ServerResponse){
  const url=new URL(req.url!,"http://fixture");
  const chunks:Buffer[]=[];req.on("data",c=>chunks.push(c));
  req.on("end",()=>{
    if(url.pathname.startsWith("/compression/")){
      const encoding=url.pathname.split("/")[2]!;const bytes=Buffer.from("compressed 中文 bytes");
      const encode:Record<string,(bytes:Buffer)=>Buffer>={gzip:gzipSync,deflate:deflateSync,raw:deflateRawSync,br:brotliCompressSync,zstd:zstdCompressSync};
      const body=encode[encoding]!(bytes);res.setHeader("content-encoding",encoding==="raw"?"deflate":encoding);res.setHeader("content-length",body.length);res.end(body);return;
    }
    if(url.pathname.startsWith("/redirect/")){res.statusCode=Number(url.pathname.split("/")[2]);res.setHeader("location","/inspect?a=1&a=2");res.end();return;}
    if(url.pathname==="/slow"){setTimeout(()=>res.end("late"),200);return;}
    if(url.pathname==="/body"){res.write("first");setTimeout(()=>res.end("last"),80);return;}
    if(url.pathname==="/status"){res.statusCode=418;res.statusMessage="Custom reason";res.setHeader("x-custom","value");res.end("failure body");return;}
    res.setHeader("content-type","application/json");res.end(JSON.stringify({method:req.method,url:req.url,a:req.headers["x-a"]??null,b:req.headers["x-b"]??null,body:Buffer.concat(chunks).toString()}));
  });
}
describe("Cordis and Rust HTTP client parity",()=>{
  const plain=createServer(target);let secure:ReturnType<typeof createSecureServer>;const sockets:any[]=[];
  let hosts:Awaited<ReturnType<typeof startHost>>[]=[];let plainUrl:string;let secureUrl:string;let directory:string;
  beforeAll(async()=>{
    const binary=fileURLToPath(new URL(`../../../target/debug/examples/server-reference${process.platform==="win32"?".exe":""}`,import.meta.url));
    const certificate=JSON.parse(execFileSync(binary,["cert"],{encoding:"utf8"}));
    directory=await mkdtemp(join(tmpdir(),"kardia-tls-"));const ca=join(directory,"ca.pem");await writeFile(ca,certificate.cert);
    secure=createSecureServer(certificate,target);
    for(const server of [plain,secure]){
      const ws=new WebSocketServer({server,perMessageDeflate:false});ws.on("connection",(socket:any)=>socket.on("message",(data:Buffer,binary:boolean)=>socket.send(data,{binary})));sockets.push(ws);
      await new Promise<void>(resolve=>server.listen(0,"127.0.0.1",resolve));
    }
    const port=(server:typeof plain|typeof secure)=>(server.address() as {port:number}).port;
    plainUrl=`http://127.0.0.1:${port(plain)}`;secureUrl=`https://localhost:${port(secure)}`;
    for(const implementation of ["upstream","rust"] as const) hosts.push(await startHost("client",implementation,{NODE_EXTRA_CA_CERTS:ca,KARDIA_HTTP_CONFIG:JSON.stringify({baseUrl:plainUrl+"/",headers:{"x-a":"default"},rootCertificates:[certificate.cert]})}));
  });
  afterAll(async()=>{await Promise.all(hosts.map(h=>h.stop()));for(const ws of sockets){for(const socket of ws.clients)socket.terminate();ws.close();}for(const server of [plain,secure]){server?.closeAllConnections();await new Promise<void>(resolve=>server?.close(()=>resolve()));}if(directory)await rm(directory,{recursive:true,force:true});});
  async function compare(args:Record<string,unknown>,endpoint="request"){
    const results=await Promise.all(hosts.map(async host=>{const r=await fetch(`${host.url}/${endpoint}`,{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(args)});expect(r.status).toBe(200);return r.json();}));expect(results[1]).toEqual(results[0]);return results[0];
  }
  it.each(["http","https"])("%s response and body",async protocol=>{const base=protocol==="http"?plainUrl:secureUrl;for(const path of ["/status","/inspect?x=a+b&x=2","/body"])await compare({url:base+path});});
  it.each(["gzip","deflate","raw","br","zstd"])("decodes %s while retaining source headers",async encoding=>{await compare({url:plainUrl+"/compression/"+encoding});});
  it.each([301,302,303,307,308])("redirect %s with bytes and stream",async code=>{for(const stream of [false,true])await compare({url:plainUrl+"/redirect/"+code,method:"POST",data:"payload",stream});});
  it("manual/error redirect policy",async()=>{for(const redirect of ["manual","error"])await compare({url:plainUrl+"/redirect/302",redirect});});
  it("timeout applies until headers, not until the end of the body",async()=>{await compare({url:plainUrl+"/slow",timeout:10});await compare({url:plainUrl+"/body",timeout:40});});
  it("intercept headers are replaced by request headers",async()=>{await compare({url:"inspect",intercept:{headers:{"x-b":"intercept"}}});await compare({url:"inspect",intercept:{headers:{"x-b":"intercept"}},headers:{"x-a":"request"}});});
  it.each(["ws","wss"])("%s text/binary roundtrip",async scheme=>{await compare({url:(scheme==="ws"?plainUrl:secureUrl).replace(/^http/,"ws")+"/ws"},"socket");});
  it("negotiates a WS subprotocol supplied through headers",async()=>{await compare({url:plainUrl.replace("http:","ws:")+"/ws",headers:{"sec-websocket-protocol":"example"}},"socket");});
  it.each(["post","pOsT","PATCH","patch","query","QUERY","trace"])("method normalization %s",async method=>{await compare({url:plainUrl+"/inspect",method});});
  it("does not decode the absent body of HEAD",async()=>{await compare({url:plainUrl+"/compression/gzip",method:"head"});});
  it.each([{"content-length":"2"},{expect:"100-continue"}])("request header failure %j",async headers=>{await compare({url:plainUrl+"/inspect",method:"DELETE",headers,timeout:100});});
});
