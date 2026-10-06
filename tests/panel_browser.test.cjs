const { test } = require('node:test');
const assert = require('node:assert/strict');
const { createServer: httpServer } = require('node:http');
const { createServer: tcpServer } = require('node:net');
const { createHash } = require('node:crypto');
const { spawn, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const panel = path.resolve(__dirname, '../xwp1/panel');
const generated = process.env.XWP1_TEST_ASSETS_DIR || panel;
const chromium = process.env.CHROMIUM || 'chromium';
const available = spawnSync(chromium, ['--version'], { stdio: 'ignore' }).status === 0;
const generatedData = fs.existsSync(path.join(generated, 'data.json'));
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function freePort() {
  const server = tcpServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}

function fixture() {
  const received = [];
  const clients = new Set();
  const server = httpServer((req, res) => {
    const name = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
    const local = path.resolve(panel, '.' + (name === '/' ? '/index.html' : name));
    if (!local.startsWith(panel + path.sep)) { res.writeHead(404).end(); return; }
    const file = fs.existsSync(local) ? local : path.join(generated, path.basename(local));
    fs.readFile(file, (err, bytes) => {
      if (err) { res.writeHead(404).end(); return; }
      const type = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.ttf': 'font/ttf' }[path.extname(file)] || 'application/octet-stream';
      res.writeHead(200, { 'Content-Type': type });
      res.end(bytes);
    });
  });
  server.on('upgrade', (req, socket) => {
    if (req.url !== '/ws' || !req.headers['sec-websocket-key']) { socket.destroy(); return; }
    const accept = createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ' + accept + '\r\n\r\n');
    clients.add(socket);
    socket.on('close', () => clients.delete(socket));
    let pending = Buffer.alloc(0);
    socket.on('data', chunk => {
      pending = Buffer.concat([pending, chunk]);
      while (pending.length >= 6) {
        let size = pending[1] & 127, head = 2;
        if (size === 126) { if (pending.length < 8) return; size = pending.readUInt16BE(2); head = 4; }
        if (size === 127) { if (pending.length < 14) return; size = Number(pending.readBigUInt64BE(2)); head = 10; }
        if (pending.length < head + 4 + size) return;
        const mask = pending.subarray(head, head + 4);
        const data = Buffer.from(pending.subarray(head + 4, head + 4 + size));
        for (let i = 0; i < size; i++) data[i] ^= mask[i % 4];
        received.push({ opcode: pending[0] & 15, data });
        pending = pending.subarray(head + 4 + size);
      }
    });
  });
  const sendText = text => {
    const bytes = Buffer.from(text);
    assert.ok(bytes.length < 126);
    for (const socket of clients) socket.write(Buffer.concat([Buffer.from([0x81, bytes.length]), bytes]));
  };
  return { server, received, clients, sendText };
}

async function until(action, description) {
  const end = Date.now() + 10000;
  while (Date.now() < end) {
    const value = await action();
    if (value) return value;
    await delay(50);
  }
  assert.fail(`timed out waiting for ${description}`);
}

test('real browser loads panel and sends keyboard notes over WebSocket', { skip: !available || !generatedData, timeout: 20000 }, async () => {
  const { server, received, clients, sendText } = fixture();
  const sockets = new Set();
  server.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  const debugPort = await freePort();
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'xwp1-browser-test-'));
  const chrome = spawn(chromium, ['--headless=new', '--no-sandbox', '--disable-gpu', '--mute-audio',
    `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, 'about:blank'], { stdio: 'ignore' });
  let devtools;
  try {
    const target = await until(async () => {
      try { return (await (await fetch(`http://127.0.0.1:${debugPort}/json`)).json()).find(item => item.type === 'page'); }
      catch { return null; }
    }, 'Chromium DevTools');
    devtools = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => { devtools.addEventListener('open', resolve, { once: true }); devtools.addEventListener('error', reject, { once: true }); });
    let nextId = 0;
    const pending = new Map();
    devtools.addEventListener('message', event => {
      const reply = JSON.parse(event.data);
      if (pending.has(reply.id)) { pending.get(reply.id)(reply); pending.delete(reply.id); }
    });
    const send = (method, params = {}) => new Promise(resolve => {
      const id = ++nextId;
      pending.set(id, resolve);
      devtools.send(JSON.stringify({ id, method, params }));
    });
    const evaluate = async expression => {
      const reply = await send('Runtime.evaluate', { expression, returnByValue: true });
      assert.ifError(reply.error);
      assert.ifError(reply.result.exceptionDetails);
      return reply.result.result.value;
    };
    await send('Page.navigate', { url: `http://127.0.0.1:${port}/` });
    await until(async () => (await evaluate("document.querySelector('#link')?.classList.contains('on') && document.querySelectorAll('#keyboard .key').length > 0")), 'panel startup');
    assert.ok(received.some(event => event.opcode === 1 && event.data.toString() === 'm?'), 'panel did not send startup control');
    sendText('S {"cpu":0.27,"peak":[0.1,0.2],"uncabled":false}');
    sendText('V 7');
    await until(async () => (await evaluate("document.querySelector('#cpu').textContent === '27%' && document.querySelector('#volText').textContent === '+7 dB'")), 'status display');
    await evaluate("document.querySelector('#vol').value = 4; document.querySelector('#vol').dispatchEvent(new Event('input', {bubbles:true}))");
    await until(() => received.some(event => event.opcode === 1 && event.data.toString() === 'v 4'), 'volume control');
    await evaluate("document.dispatchEvent(new KeyboardEvent('keydown', {key:'a', code:'KeyA', bubbles:true}))");
    await evaluate("document.dispatchEvent(new KeyboardEvent('keyup', {key:'a', code:'KeyA', bubbles:true}))");
    await until(() => received.some(event => event.opcode === 2 && event.data.equals(Buffer.from([0x80, 60, 0]))), 'note off');
    const notes = received.filter(event => event.opcode === 2 && [0x90, 0x80].includes(event.data[0])).map(event => [...event.data]);
    assert.deepEqual(notes.slice(-2), [[0x90, 60, 100], [0x80, 60, 0]]);
  } finally {
    devtools?.close();
    chrome.kill();
    for (const socket of clients) socket.destroy();
    for (const socket of sockets) socket.destroy();
    await new Promise(resolve => server.close(resolve));
    fs.rmSync(profile, { recursive: true, force: true });
  }
});
