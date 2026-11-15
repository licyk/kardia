import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

const upstream = process.argv[2];
const load = (name) => import(pathToFileURL(resolve(upstream, `packages/${name}/lib/index.js`)).href);
const { Context } = await load('core');
const { default: Server } = await load('server/core');
const scenario = process.argv[3];
const ctx = new Context();
await ctx.use(Server, { port: 0 });
let target;
if (scenario === 'echo' || scenario === 'proxy') {
  if (scenario === 'proxy') {
    const { default: Http } = await load('http');
    const Proxy = await load('server/proxy');
    target = new Context();
    await target.use(Server, { port: 0 });
    await target.use(await load('server/echo'));
    await ctx.use(Http);
    await ctx.use(Proxy, { target: target.server.publicUrl });
  } else {
    await ctx.use(await load('server/echo'));
  }
} else {
  ctx.server.get('/hello', async (_, res) => { res.text('world'); });
  ctx.server.get('/params/:name', async (req, res) => { res.text(req.params.name); });
  ctx.server.get('/query', async (req, res) => { res.json([...req.query]); });
  ctx.server.post('/body', async (req, res) => { res.body = req.body; });
  ctx.server.get('/status', async (_, res) => { res.status = 204; });
  ctx.server.get('/error', async () => { throw new Error('fixture error'); });
  ctx.server.get('/claimed-error', async (_, res) => { res.status = 503; throw new Error('fixture error'); });
  ctx.server.get('/stream', async (_, res) => {
    res.body = new ReadableStream({ start(c) { c.enqueue(new TextEncoder().encode('hello')); c.enqueue(new TextEncoder().encode(' world')); c.close(); } });
    const reads = await Promise.all([res.text(), res.text()]);
    res.headers.set('x-inspected', reads.join('|'));
  });
  ctx.server.get('/next', async (_, __, next) => next());
  ctx.server.get('/next', async () => Response.json({ ok: true }));
  ctx.server.all('/allow', async (_, __, next) => next());
  ctx.intercept('server', { path: '/api' }).server.get('/scoped', async (_, res) => { res.text('scoped'); });
  ctx.server.ws('/ws', async (_, accept) => {
    const socket = await accept();
    socket.on('message', (data, binary) => socket.send(data, { binary }));
  });
}
const lines = createInterface({ input: process.stdin });
console.log(JSON.stringify({ url: ctx.server.publicUrl }));
for await (const line of lines) {
  const command = JSON.parse(line);
  if (command.op === 'stop') break;
}
await ctx.fiber.dispose();
await target?.fiber.dispose();
