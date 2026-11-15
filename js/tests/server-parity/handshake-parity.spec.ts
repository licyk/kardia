import {beforeAll,afterAll,describe,it,expect} from 'vitest';
import {request} from 'node:http';
import {startHost} from './host.ts';
describe('Cordis and Rust WebSocket handshake parity',()=>{
  let hosts:Awaited<ReturnType<typeof startHost>>[]=[];
  beforeAll(async()=>{for(const implementation of ['upstream','rust'] as const)hosts.push(await startHost('core',implementation));});
  afterAll(async()=>{await Promise.all(hosts.map(h=>h.stop()));});
  const cases:[string,string,Record<string,string|undefined>][]=[
    ['valid','GET',{}],['version8','GET',{'sec-websocket-version':'8'}],['numeric version','GET',{'sec-websocket-version':'013'}],
    ['wrong version','GET',{'sec-websocket-version':'12'}],['missing version','GET',{'sec-websocket-version':undefined}],
    ['bad key','GET',{'sec-websocket-key':'invalid'}],['missing key','GET',{'sec-websocket-key':undefined}],
    ['bad protocol','GET',{'sec-websocket-protocol':'a b'}],['duplicate protocol','GET',{'sec-websocket-protocol':'a,a'}],['empty protocol','GET',{'sec-websocket-protocol':''}],
    ['protocol list','GET',{'sec-websocket-protocol':'a, b'}],['unsupported extensions','GET',{'sec-websocket-extensions':'invalid; extension===broken'}],
    ['wrong method','POST',{}],['wrong upgrade','GET',{upgrade:'other'}],['no connection upgrade','GET',{connection:'close'}],
  ];
  it.each(cases)('%s',async(_,method,overrides)=>{
    const headers:Record<string,string>={connection:'Upgrade',upgrade:'websocket','sec-websocket-key':'dGhlIHNhbXBsZSBub25jZQ==','sec-websocket-version':'13'};
    for(const [key,value] of Object.entries(overrides)){if(value===undefined)delete headers[key];else headers[key]=value;}
    const results=await Promise.all(hosts.map(host=>new Promise((resolve,reject)=>{
      const req=request(host.url+'/ws',{method,headers});req.on('error',reject);req.setTimeout(3000,()=>req.destroy(Error('handshake timeout')));
      const result=(res:any,body:string)=>({status:res.statusCode,reason:res.statusMessage,headers:Object.fromEntries(['content-type','content-length','sec-websocket-version','sec-websocket-protocol','sec-websocket-extensions'].map(k=>[k,res.headers[k]??null])),body});
      req.on('upgrade',(res,socket)=>{socket.destroy();resolve(result(res,''));});req.on('response',res=>{const chunks:Buffer[]=[];res.on('data',c=>chunks.push(c));res.on('end',()=>resolve(result(res,Buffer.concat(chunks).toString())));});req.end();
    })));expect(results[1]).toEqual(results[0]);
  });
});
