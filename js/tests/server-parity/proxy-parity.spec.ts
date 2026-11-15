import { beforeAll,afterAll,describe,it,expect } from 'vitest';
import { createServer,request } from 'node:http';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';
import { startHost } from './host.ts';
import { upstream } from './upstream.ts';
const {WebSocket,WebSocketServer}=createRequire(join(upstream,'package.json'))('ws');
describe('Cordis and Rust reverse proxy parity',()=>{
  const target=createServer((req,res)=>{const chunks:Buffer[]=[];req.on('data',c=>chunks.push(c));req.on('end',()=>{
    const path=new URL(req.url!,'http://target').pathname;
    if(path.endsWith('/gzip')){const body=gzipSync('compressed 中文');res.setHeader('content-encoding','gzip');res.setHeader('content-length',body.length);res.setHeader('x-custom','compressed');res.end(body);return;}
    if(path.endsWith('/redirect')){res.writeHead(302,{location:'/redirected?q=a%20b&q=2'});res.end();return;}
    if(path.endsWith('/status')){res.statusCode=418;res.statusMessage='Custom upstream reason';}
    if(path.endsWith('/absent'))res.statusCode=404;
    res.setHeader('x-custom','value');res.setHeader('set-cookie',['a=1','b=2']);res.setHeader('content-type','application/json');
    res.end(JSON.stringify({path:req.url,method:req.method,custom:req.headers['x-request']??null,body:Buffer.concat(chunks).toString()}));
  });});
  const wss=new WebSocketServer({server:target,perMessageDeflate:false});
  const closed:{code:number,reason:string}[]=[];
  wss.on('connection',(ws:any,req:any)=>{const url=new URL(req.url,'http://target');ws.on('close',(code:number,reason:Buffer)=>closed.push({code,reason:reason.toString()}));ws.on('message',(data:Buffer,binary:boolean)=>{
    if(data.toString()==='close'){ws.close(Number(url.searchParams.get('code')),'reason 中文');return;}
    if(data.toString()==='break'){ws.terminate();return;}
    ws.send(data,{binary});
  });});
  let hosts:Awaited<ReturnType<typeof startHost>>[]=[];
  beforeAll(async()=>{await new Promise<void>(r=>target.listen(0,'127.0.0.1',r));const port=(target.address() as {port:number}).port;for(const impl of ['upstream','rust'] as const)hosts.push(await startHost('proxy',impl,{KARDIA_PROXY_TARGET:`http://127.0.0.1:${port}/base///`,KARDIA_PROXY_PATH:'/proxy'}));});
  afterAll(async()=>{await Promise.all(hosts.map(h=>h.stop()));for(const ws of wss.clients)ws.terminate();wss.close();target.closeAllConnections();await new Promise<void>(r=>target.close(()=>r()));});
  async function compare(path:string,method='GET',body?:string){
    const results=await Promise.all(hosts.map(h=>new Promise((resolve,reject)=>{
      const req=request(h.url+path,{method,headers:{'x-request':'sent',...(body?{'content-length':Buffer.byteLength(body),'content-type':'application/json'}:{})}},res=>{const chunks:Buffer[]=[];res.on('data',c=>chunks.push(c));res.on('error',reject);res.on('end',()=>resolve({status:res.statusCode,reason:res.statusMessage,headers:Object.fromEntries(['content-type','content-encoding','content-length','x-custom','set-cookie'].map(k=>[k,res.headers[k]??null])),body:Buffer.concat(chunks).toString()}));});req.on('error',reject);req.end(body);
    })));expect(results[1]).toEqual(results[0]);return results[0];
  }
  it.each(['/proxy','/proxy/','/proxy/api/users/42','/proxy/search?q=a%20b&q=x%2By&blank','/proxy/a%2Fb','/proxy/%E4%B8%AD%E6%96%87','/proxy/status','/proxy/absent','/proxy/headers','/proxy/gzip','/proxy/redirect'])('HTTP %s',async path=>{await compare(path);});
  it.each(['POST','PUT','PATCH','DELETE','OPTIONS','HEAD'])('method/body %s',async method=>{await compare('/proxy/echo',method,method==='HEAD'?undefined:'{"foo":"bar"}');});
  it('public URL resolves the providing context',async()=>{await compare('/__proxy_url');});
  async function websocket(host:typeof hosts[number],suffix:string,action:string){return new Promise((resolve,reject)=>{
    const ws=new WebSocket(host.url.replace('http','ws')+'/proxy/ws'+suffix,['alpha','beta']);const messages:any[]=[];
    const timer=setTimeout(()=>{ws.terminate();reject(Error('WS timed out'));},3000);
    ws.on('error',reject);ws.on('open',()=>{if(action==='roundtrip'){ws.send('hello 中文');ws.send(Buffer.from([0,255]));}else ws.send(action);});
    ws.on('message',(b:Buffer,binary:boolean)=>{messages.push({binary,bytes:[...b]});if(messages.length===2)ws.close(4001,'client done');});
    ws.on('close',(code:number,reason:Buffer)=>{clearTimeout(timer);resolve({messages,code,reason:reason.toString(),protocol:ws.protocol});});
  });}
  it('WS text/binary, subprotocol and downstream close',async()=>{const results=[];for(const host of hosts)results.push(await websocket(host,'','roundtrip'));expect(results[1]).toEqual(results[0]);await new Promise(r=>setTimeout(r,20));expect(closed.slice(-2)).toEqual([{code:4001,reason:'client done'},{code:4001,reason:'client done'}]);});
  it.each([1000,1001,1011,4002])('WS upstream close %s',async code=>{const results=[];for(const host of hosts)results.push(await websocket(host,`?code=${code}`,'close'));expect(results[1]).toEqual(results[0]);});
  it('WS upstream transport error',async()=>{const results=[];for(const host of hosts)results.push(await websocket(host,'','break'));expect(results[1]).toEqual(results[0]);});
});
it('unavailable upstream remains a request failure and never accepts the WS handshake',async()=>{
  const probe=createServer();await new Promise<void>(r=>probe.listen(0,'127.0.0.1',r));const port=(probe.address() as {port:number}).port;await new Promise<void>(r=>probe.close(()=>r()));
  const hosts:Awaited<ReturnType<typeof startHost>>[]=[];
  try {
    for(const impl of ['upstream','rust'] as const)hosts.push(await startHost('proxy',impl,{KARDIA_PROXY_TARGET:`http://127.0.0.1:${port}`}));
    const results=[];
    for(const host of hosts){const http=await fetch(host.url+'/unavailable');expect(http.status).toBe(500);await http.arrayBuffer();results.push(await new Promise(resolve=>{const ws=new WebSocket(host.url.replace('http','ws')+'/unavailable');let opened=false;let response:number|undefined;ws.on('open',()=>{opened=true;ws.close();});ws.on('unexpected-response',(_:unknown,res:any)=>{response=res.statusCode;res.resume();ws.terminate();});ws.on('error',()=>{});ws.on('close',(code:number)=>resolve({opened,response,code}));}));}
    expect(results[1]).toEqual(results[0]);expect(results[0]).toEqual({opened:false,response:undefined,code:1006});
  } finally {await Promise.all(hosts.map(h=>h.stop()));}
});
