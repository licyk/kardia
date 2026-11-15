import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { createHash } from "node:crypto";
import { copyFile, mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { request } from "node:http";
import { startHost } from "./host.ts";
import { upstream } from "./upstream.ts";

type Host=Awaited<ReturnType<typeof startHost>>;
async function compare(hosts:Host[],path:string) {
  const results=await Promise.all(hosts.map(host=>new Promise((resolve,reject)=>{
    const req=request(host.url,{path},res=>{
      const chunks:Buffer[]=[];res.on("data",chunk=>chunks.push(chunk));res.on("error",reject);
      res.on("end",()=>resolve({status:res.statusCode,reason:res.statusMessage,headers:Object.fromEntries(["content-type","content-disposition","location","allow"].map(k=>[k,res.headers[k]])),body:createHash("sha256").update(Buffer.concat(chunks)).digest("hex")}));
    });req.on("error",error=>reject(new Error(`${host.url}${path}: ${error.message}`)));req.end();
  })));
  expect(results[1],path).toEqual(results[0]);
  return results[0] as {headers:Record<string,string>};
}
async function pair(config:Record<string,unknown>) {
  const hosts:Host[]=[];
  try {for(const implementation of ["upstream","rust"] as const) hosts.push(await startHost("static",implementation,{KARDIA_STATIC_CONFIG:JSON.stringify(config)}));}
  catch(error){await Promise.all(hosts.map(h=>h.stop()));throw error;}
  return hosts;
}

describe("Static real files and configuration parity",()=>{
  let directory:string;
  beforeAll(async()=>{
    directory=await mkdtemp(join(tmpdir(),"kardia-static-"));
    await mkdir(join(directory,"sub"));await mkdir(join(directory,"errors"));
    for(const [name,body] of Object.entries({"hello.html":"<h1>Hello</h1>\n","noext":"plain text\n","page":"exact","page.html":"extension","index.htm":"second","index.html":"first","sub/index.html":"sub index","errors/404.html":"missing",".secret":"hidden","中文 (a)!.txt":"unicode","empty":"","html":"plain"})) await writeFile(join(directory,name),body);
  });
  afterAll(async()=>{await rm(directory,{recursive:true,force:true});});
  it("compares native FS failure codes", async()=>{
    const hosts=await pair({root:pathToFileURL(directory).href});
    try {for(const path of ["missing","hello.html/child","hello.html/","./","%2Fbad"]) await compare(hosts,"/__file-error?path="+encodeURIComponent(path));}
    finally {await Promise.all(hosts.map(h=>h.stop()));}
  });
  it("keeps error-page resolution separate from containment and exclude", async()=>{
    const outer=await mkdtemp(join(tmpdir(),"kardia-error-page-"));
    await mkdir(join(outer,"root"));await writeFile(join(outer,"outside.html"),"outside error page");
    const root=pathToFileURL(join(outer,"root")).href;
    try {
      for(const options of [{index:["../outside.html"]},{exclude:["**"],disposition:"attachment",errorPages:{404:"../outside.html"}}]) {
        const hosts=await pair({root,...options});
        try {await compare(hosts,"/");await compare(hosts,"/missing");}
        finally {await Promise.all(hosts.map(h=>h.stop()));}
      }
    } finally {await rm(outer,{recursive:true,force:true});}
  });
  it.each([
    {},{extensions:[".html",".htm"]},{index:["index.htm","index.html"]},{redirect:false},
    {fallthrough:true},{errorPages:{404:"errors/404.html"}},{disposition:"attachment",errorPages:{404:"errors/404.html"}},
    {disposition:"inline"},{exclude:["**/.*"]},{exclude:["*.html","noext"]},{exclude:["sub/**"]},
    {exclude:["**/*.{html,htm}"]},{exclude:["**/!(noext)"]},{exclude:["**/[a-z]*"]},
    {exclude:["!hello.html"]},{exclude:["**/@(hello|page).html"]},{exclude:["**/+(hello|page).html"]},
    {exclude:["**/h?llo.*"]},{exclude:["**/[!a-z]*"]},{exclude:["**/[[:alpha:]]*"]},
    {exclude:["**/{hello,page}.html"]},{exclude:["./hello.html"]},{exclude:["**/*.HTML"]},
    {exclude:["**/中文*"]},{exclude:["**/\\.secret"]},{exclude:["**/hello.(html|txt)"]},
  ])("configuration %j",async options=>{
    const hosts=await pair({root:pathToFileURL(directory).href,...options});
    try {for(const path of ["/","/hello.html","/hello","/missing","/noext","/page","/sub","/sub?x=y","/sub/","/sub/index.html","/.secret","/empty","/html","/%E4%B8%AD%E6%96%87%20(a)!.txt","/..%2Fescape"]) await compare(hosts,path);}
    finally {await Promise.all(hosts.map(h=>h.stop()));}
  });
});

describe("Static complete file-type 20.5.0 fixture corpus",()=>{
  let hosts:Host[]=[];let directory:string;
  const fixtures=join(upstream,".file-type-reference","fixture");
  let names:string[]=[];
  beforeAll(async()=>{
    names=(await readdir(fixtures,{withFileTypes:true})).filter(e=>e.isFile()).map(e=>e.name).sort();
    directory=await mkdtemp(join(tmpdir(),"kardia-mime-"));
    for(let i=0;i<names.length;i++) await copyFile(join(fixtures,names[i]!),join(directory,`sample${i}`));
    const asf=Buffer.alloc(70);
    Buffer.from([0x30,0x26,0xb2,0x75,0x8e,0x66,0xcf,0x11,0xa6,0xd9]).copy(asf);
    Buffer.from([0x91,7,0xdc,0xb7,0xb7,0xa9,0xcf,0x11,0x8e,0xe6,0,0xc0,0xc,0x20,0x53,0x65]).copy(asf,30);
    asf.writeBigUInt64LE(40n,46);
    await writeFile(join(directory,`sample${names.length}`),asf);names.push("supplement-generic-asf");
    await writeFile(join(directory,`sample${names.length}`),Buffer.concat([Buffer.alloc(4),Buffer.from("ftyphevc")]));names.push("supplement-heic-sequence");
    hosts=await pair({root:pathToFileURL(directory).href});
  });
  afterAll(async()=>{await Promise.all(hosts.map(h=>h.stop()));if(directory)await rm(directory,{recursive:true,force:true});});
  it("compares status, MIME and every body byte for every fixture without extension fallback",async()=>{
    const differences:string[]=[];
    const covered=new Set<string>();
    for(let i=0;i<names.length;i++) {try {const result=await compare(hosts,`/sample${i}`);covered.add(result.headers["content-type"]!);} catch(error){differences.push(`${names[i]}: ${String(error)}`);}}
    expect(differences).toEqual([]);
    const {supportedMimeTypes}=await import(pathToFileURL(join(upstream,"node_modules/file-type/core.js")).href);
    expect([...supportedMimeTypes as Set<string>].filter(type=>!covered.has(type))).toEqual([]);
    expect(names.length).toBeGreaterThan(150);
    console.log(`Compared ${names.length} upstream file-type fixtures through both real static servers`);
  },120_000);
});
