// XW-P1 Solo Synth panel. Talks to xwp1-rt over one WebSocket: MIDI both
// ways (parameter SysEx, notes, program changes), status, and the sound when
// this page asks for it. Every value shown here is read back from the
// firmware; edits go to its edit buffer only.
'use strict';

const $ = (sel, root = document) => root.querySelector(sel);
const SVG = 'http://www.w3.org/2000/svg';
function el(tag, attrs = {}, ...kids) {
  const svg = ['svg', 'path', 'circle', 'line', 'g', 'text', 'defs', 'radialGradient', 'stop', 'rect', 'polyline'].includes(tag);
  const node = svg ? document.createElementNS(SVG, tag) : document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') node.setAttribute('class', v);
    else if (k === 'text') node.textContent = v;
    else if (k.startsWith('on')) node.addEventListener(k.slice(2), v);
    else if (v !== false && v != null) node.setAttribute(k, v);
  }
  for (const kid of kids.flat(Infinity)) if (kid != null) node.append(kid);
  return node;
}
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

// ---------------------------------------------------------------- data

let D;                          // data.json
const P = {};                   // Solo Synth parameter id -> table row
const refs = new Map();         // key -> ref (everything the firmware can be asked for)
const vals = new Map();         // key -> value
const subs = new Map();         // key -> Set of callbacks
const keyOf = (ct, pid, inst, ai) => `${ct}:${pid}:${inst}:${ai}`;

function makeRef(ct, pid, inst, ai, o) {
  const key = keyOf(ct, pid, inst, ai);
  let ref = refs.get(key);
  if (!ref) { ref = { key, ct, pid, inst, ai, ...o }; refs.set(key, ref); }
  return ref;
}
function solo(id, inst = 0) {
  const p = P[id];
  let { min, max } = p;
  if (p.vt === 'wf') max = [310, 310, 2157, 2157, 0, 13][inst];
  // pitch offset: 16 bits signed, 512 per semitone; the knob moves in semitones, with Shift in the instrument's own steps of 48
  if (p.vt === 'pk') { min = -24 * 512; max = 24 * 512; }
  return makeRef(9, p.pid, inst, p.ai || 0, { vt: p.vt, min, max, name: p.name, labels: p.enum ? D.enums[p.enum] : null, id,
                                      step: p.vt === 'pk' ? 512 : 1, fine: p.vt === 'pk' ? 48 : 1 });
}
const mixer = (name, label) => makeRef(2, D.mixer[name], 0, 0, { vt: 'nf', min: 0, max: 127, name: label });
const system = (name, label, max = 127) => makeRef(2, D.system[name], 0, 0, { vt: 'nf', min: 0, max, name: label });
const dspAlgorithm = () => makeRef(0x13, 2, 0, 0, { vt: 'u14', min: 0, max: 16383, name: 'Effect' });
const dspParam = i => makeRef(0x13, 3, 0, i, { vt: 'nf', min: 0, max: 127, name: `Effect parameter ${i + 1}` });
const toneNumber = () => makeRef(2, 0x69, 0, 0, { vt: 'u14', min: 0, max: 16383, name: 'Tone' });

function encode(ref, v) {
  switch (ref.vt) {
    case 'hx': { let w = v; return Array.from({ length: ref.bytes }, () => { const b = w & 127; w = Math.floor(w / 128); return b; }); }
    case 'hxwf': return [v & 127, (v >> 7) & 127, (v >> 14) & 127];
    case 'nf': return [v];
    case 'cf': return [v + 64];
    case 'cF': { const w = v + 128; return [w & 127, w >> 7]; }
    case 'tn': { const w = 2 * (v + 256); return [w & 127, w >> 7]; }
    case 'u14': return [v & 127, v >> 7];
    case 'pk': { const w = v & 0xFFFF; return [w & 127, (w >> 7) & 127, w >> 14]; }     // only 0..3 is accepted in the third byte
    case 'wf': { const w = v + D.waveBase[ref.inst]; return [w & 127, (w >> 7) & 127, 0]; }
  }
  return [v];
}
function decode(ref, b) {
  const w = (b[0] || 0) | ((b[1] || 0) << 7);
  switch (ref.vt) {
    case 'hx': return w | ((b[2] || 0) << 14);
    case 'hxwf': return w | ((b[2] || 0) << 14);
    case 'nf': return b[0];
    case 'cf': return b[0] - 64;
    case 'cF': return w - 128;
    case 'tn': return w / 2 - 256;
    case 'u14': return w;
    case 'pk': { const u = (w | ((b[2] & 3) << 14)) & 0xFFFF; return u & 0x8000 ? u - 0x10000 : u; }  // negative comes back with 0x7f there
    case 'wf': return w - D.waveBase[ref.inst];
  }
  return b[0];
}
const address = ref => [ref.ct, 0, 0, 0, 0, 0, 0, 0, 0, 0, ref.inst, 0, ref.pid & 127, ref.pid >> 7, ref.ai, 0, 0, 0];
const setMessage = (ref, v) => [0xF0, 0x44, 0x16, 0x03, 0x7F, 1, ...address(ref), ...encode(ref, v), 0xF7];
const requestMessage = ref => [0xF0, 0x44, 0x16, 0x03, 0x7F, 0, ...address(ref), 0xF7];

function watch(ref, fn) {
  if (!subs.has(ref.key)) subs.set(ref.key, new Set());
  subs.get(ref.key).add(fn);
  fn(vals.get(ref.key));
  return () => subs.get(ref.key).delete(fn);
}
function forget(ref) {
  if (!vals.has(ref.key)) return;
  vals.delete(ref.key);
  for (const fn of subs.get(ref.key) || []) fn(undefined);
}
function store(ref, v) {
  if (vals.get(ref.key) === v) return;
  vals.set(ref.key, v);
  for (const fn of subs.get(ref.key) || []) fn(v);
}

// Edits: the store changes at once, the SysEx goes out at most every 40 ms per parameter
// (an envelope corner moves two: 50 writes a second, and the instrument takes 58).
const pendingSend = new Map();
function edit(ref, v) {
  v = clamp(Math.round(v), ref.min, ref.max);
  const old = vals.get(ref.key);
  if (old == null || old === v) return;     // not read yet (or stale after a preset change): nothing to edit
  // a read of this value still under way would bring the old value back
  const f = inflight.get(ref.key);
  if (f) f.ignore = true;
  queue = queue.filter(r => r !== ref);
  spot.delete(ref.key);
  store(ref, v);
  later.delete(ref.key);
  if (pendingSend.has(ref.key)) pendingSend.get(ref.key).v = v;
  else {
    sendMidi(setMessage(ref, v));
    const slot = { v, sent: v };
    pendingSend.set(ref.key, slot);
    slot.timer = setTimeout(() => {
      pendingSend.delete(ref.key);
      if (slot.v !== slot.sent) sendMidi(setMessage(ref, slot.v));
    }, 40);
  }
  for (const fn of editHooks) fn(ref, v);
}
const editHooks = [];           // called after an edit made by hand (macro.js: linked envelopes)

// Edits made many at a time (a macro moves up to a few hundred parameters):
// the store changes at once, the SysEx goes out in turn, latest value only.
// The instrument takes 58 parameter writes a second (measured on the
// emulator); more than that only queues up behind the link. Now and then
// the firmware loses a write out of a long run (2 of about 150, seen once,
// not reproduced), so what was written is read back when things are quiet
// and a value that did not arrive is sent again.
// A linked envelope dragged by hand sends its own values at once and the
// other blocks' through here: the loop waits while more than 50 writes of
// any kind went out in the last second, or notes would wait behind them.
const later = new Map();        // key -> ref, in the order they wait
const sets = [];                // when the last parameter writes went out
const written = new Map();      // key -> ref: sent by the loop below, not read back yet
const checking = new Map();     // key -> times sent again, for the read-backs under way
let lastLater = 0;
function discardToneEdits() {
  for (const slot of pendingSend.values()) clearTimeout(slot.timer);
  pendingSend.clear();
  for (const edits of [later, written, checking]) edits.clear();
}
function editLater(ref, v) {
  v = clamp(Math.round(v), ref.min, ref.max);
  const old = vals.get(ref.key);
  if (old == null || old === v) return;
  const f = inflight.get(ref.key);
  if (f) f.ignore = true;
  queue = queue.filter(r => r !== ref);
  spot.delete(ref.key);
  store(ref, v);
  if (pendingSend.has(ref.key)) pendingSend.get(ref.key).v = v;
  else later.set(ref.key, ref);
}
setInterval(() => {
  while (sets.length && sets[0] < performance.now() - 1000) sets.shift();
  if (sets.length >= 50) return;
  for (const [key, ref] of later) {
    later.delete(key);
    const v = vals.get(key);
    if (v == null) continue;      // the tone changed meanwhile
    sendMidi(setMessage(ref, v));
    written.set(key, ref);
    lastLater = performance.now();
    return;
  }
  if (!written.size || queue.length || inflight.size || performance.now() - lastLater < 300) return;
  for (const [key, ref] of written) {
    if (!checking.has(key)) checking.set(key, 0);
    queue.push(ref);
  }
  written.clear();
  pump();
}, 20);
// The answer to such a read-back. Twice sent again and still different: the firmware's value is the true one.
function checked(ref, got) {
  const want = vals.get(ref.key), tries = checking.get(ref.key);
  checking.delete(ref.key);
  if (want == null || got === want || later.has(ref.key) || pendingSend.has(ref.key)) return;
  if (tries >= 2) { store(ref, got); return; }
  checking.set(ref.key, tries + 1);
  later.set(ref.key, ref);
}

// ---------------------------------------------------------------- link

let ws = null, linked = false;
function connect() {
  ws = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws');
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    linked = true;
    $('#link').classList.add('on');
    if (listening) ws.send('a 1');
    ws.send('m?');
    ws.send('b?');
    ws.send('w?');
    ws.send('v?');
    ws.send('k?');
    sendMidi([0xF0, 0x7D, 0x58, 0x54, poly.part, 0xF7]);
    if (window.frontLinked) window.frontLinked();
    if (window.storeLinked) window.storeLinked();
    if (window.macros) window.macros.syncLfo();
    readPatch(true);
  };
  ws.onclose = () => {
    linked = false;
    $('#link').classList.remove('on');
    $('#presetName').textContent = 'reconnecting';
    setTimeout(connect, 1000);
  };
  ws.onmessage = ev => {
    if (typeof ev.data === 'string') {
      if (ev.data[0] === 'S') status(JSON.parse(ev.data.slice(2)));
      else if (ev.data[0] === 'A') { stream.rate = +ev.data.slice(2); stream.playAt = 0; }
      else if (ev.data[0] === 'P') controllers(JSON.parse(ev.data.slice(2)));
      else if (ev.data[0] === 'B') polyphony(ev.data.split(' '));
      else if (ev.data[0] === 'W' && window.macros) window.macros.waveReady(ev.data === 'W 1');
      else if (ev.data[0] === 'V') showVolume(Number(ev.data.slice(2)));
      else if (ev.data[0] === 'K') {
        keysOn = ev.data[2] !== '0'; keysEach = ev.data[2] === '2';
        if (window.playerKeys) window.playerKeys(ev.data);
        for (const fn of keyWatch) fn();
        if (midiOpen) midiPopover();
      }
      return;
    }
    const bytes = new Uint8Array(ev.data);
    if (bytes[0] === 0x41) play(ev.data);
    else if (bytes[0] === 0x4D) fromSynth(bytes.subarray(1));
    else if (bytes[0] === 0x49) fromElsewhere(bytes.subarray(1));
  };
}
function sendMidi(bytes) {
  if (bytes[0] === 0xF0 && bytes[5] === 1) sets.push(performance.now());
  if (linked) ws.send(new Uint8Array(bytes));
}

// A parameter message from the instrument (an answer or an echo of a set).
function parameter(msg) {
  if (msg.length < 26 || msg[0] !== 0xF0 || msg[1] !== 0x44 || msg[5] !== 1) return null;
  const a = msg.subarray(6, 24);
  const ref = refs.get(keyOf(a[0], a[12] | (a[13] << 7), a[10], a[14]));
  if (!ref) return null;
  const value = decode(ref, msg.subarray(24, msg.length - 1));
  // The PCM selector wrote this value directly. Its own reload deliberately
  // skips the tone-number read, so an older reply cannot replace the choice.
  if (window.pcmToneTarget != null && ref.key === toneNumber().key && value !== window.pcmToneTarget) return null;
  store(ref, value);
  return ref;
}
const synthTaps = [];           // pages that read values of their own: tap(msg) -> true when the message was theirs
window.synthTaps = synthTaps;
function fromSynth(msg) {
  for (const tap of synthTaps) if (tap(msg)) return;
  if (msg[1] === 0x7D) { if (msg[3] === 0x4C) { if (window.frontPanel) window.frontPanel(msg); } else peeked(msg); return; }
  if (msg.length < 26 || msg[0] !== 0xF0 || msg[1] !== 0x44 || msg[5] !== 1) return;
  const a = msg.subarray(6, 24), key = keyOf(a[0], a[12] | (a[13] << 7), a[10], a[14]);
  const f = inflight.get(key);
  if (spot.has(key)) {
    // one of the values taken from memory, asked of the firmware as well: if it differs, the map is not to be trusted
    const want = spot.get(key), ref = refs.get(key);
    spot.delete(key);
    if (decode(ref, msg.subarray(24, msg.length - 1)) !== want) {
      spot.clear();
      for (const r of peekedRefs) if (!queue.includes(r)) { forget(r); queue.push(r); total++; }
      peekedRefs = [];
    }
  }
  if (f && checking.has(key)) checked(f.ref, decode(f.ref, msg.subarray(24, msg.length - 1)));
  else if (!f || !f.ignore) parameter(msg);
  if (f) {
    inflight.delete(key); done++;
    if (f.refresh) { queue.unshift(f.ref); total++; }
    pump();
  }
}
// Another page, or something on the ALSA port, played or edited: follow along.
function fromElsewhere(bytes) {
  let i = 0;
  while (i < bytes.length) {
    const s = bytes[i];
    if (s === 0xF0) {
      let j = i;
      while (j < bytes.length && bytes[j] !== 0xF7) j++;
      const msg = bytes.subarray(i, j + 1);
      const toneChange = msg[5] === 1 && msg[6] === 2 && msg[18] === 0x69 && msg[19] === 0;
      if (toneChange) {
        discardToneEdits();
        window.pcmToneTarget = null;
        for (const ref of refs.values()) if (ref !== toneNumber()) forget(ref);
        queue = []; inflight.clear();
      }
      if (msg[5] === 1) parameter(msg);
      if (toneChange) {
        clearTimeout(reloadTimer);
        reloadTimer = setTimeout(() => readPatch(false), 400);
      }
      i = j + 1;
    } else if ((s & 0xF0) === 0xC0) {
      discardToneEdits();
      window.pcmToneTarget = null;
      for (const ref of refs.values()) forget(ref);
      clearTimeout(reloadTimer); reloadTimer = setTimeout(() => readPatch(true), 400); i += 2;
    } else if ((s & 0xF0) === 0xB0) {
      const [cc, v] = [bytes[i + 1], bytes[i + 2]];
      if (cc === 7) store(mixer('volume'), v);
      if (cc === 10) store(mixer('pan'), v);
      if (cc === 91) store(mixer('reverb_send'), v);
      window.macros.controller(cc, v);
      i += 3;
    } else {
      if ((s & 0xE0) === 0x80) paintKey(bytes[i + 1], (s & 0xF0) === 0x90 && bytes[i + 2] > 0);
      i += (s & 0xF0) === 0xD0 ? 2 : s >= 0x80 ? 3 : 1;
    }
  }
}
let reloadTimer = 0;

// Reading the whole patch back: a few requests in flight at a time, each
// repeated if its answer does not come. That is 32 values a second, 13 s for
// a Solo tone, so where mem.json (tools/tone_map.py) says in which memory
// cell the firmware keeps a value, the emulator is asked for that memory
// instead (one request, answered at once). A dozen of those values are then
// asked of the firmware too; a difference, and all are read the slow way.
let cells = null, spans = [];   // Solo mem.json (also used by the panel load check)
const otherCells = {};
let peekCells = [], peekSpans = [];
let peeking = null;             // the memory asked for: { left, mem, timer }
let peekedRefs = [];
const spot = new Map();         // key -> the value memory gave, for the ones being checked
const WIRE_BYTES = { nf: 1, cf: 1, cF: 2, tn: 2, u14: 2, pk: 3, wf: 3, hxwf: 3 };
function spansOf(cells) {
  const result = [];
  for (const addr of cells.map(c => c[4]).sort((a, b) => a - b)) {
    const last = result[result.length - 1];
    if (last && addr - last[1] < 64) last[1] = addr + 2; else result.push([addr, addr + 2]);
  }
  return result;
}
function mapped(data) {
  cells = data.cells;
  spans = spansOf(cells);
}
function peek() {
  const seven = (v, n) => Array.from({ length: n }, (_, i) => Math.floor(v / 128 ** i) % 128);
  peeking = { left: peekSpans.length, ranges: new Set(peekSpans.map(([from]) => from)), mem: new Map(),
    timer: setTimeout(() => { peeking = null; pump(); }, 500) };    // no answer: a player without this
  sendMidi([0xF0, 0x7D, 0x58, 0x50, ...peekSpans.flatMap(([from, to]) => [...seven(from, 5), ...seven(to - from, 2)]), 0xF7]);
}
function peeked(msg) {
  if (!peeking || msg[2] !== 0x58 || msg[3] !== 0x50) return;
  const addr = msg.subarray(4, 9).reduceRight((a, b) => a * 128 + b, 0);
  if (!peeking.ranges.delete(addr)) return;  // an older tab's answer, or a duplicate
  for (let i = 9, n = 0; i + 1 < msg.length - 1; i += 2, n++) peeking.mem.set(addr + n, msg[i] << 4 | msg[i + 1]);
  if (--peeking.left > 0) return;
  clearTimeout(peeking.timer);
  const mem = peeking.mem, waiting = new Set(queue);
  peeking = null;
  peekedRefs = [];
  for (const [ct, pid, inst, ai, at, kind, a, b] of peekCells) {
    const ref = refs.get(keyOf(ct, pid, inst, ai)), lo = mem.get(at), hi = mem.get(at + 1), n = ref && (ref.bytes || WIRE_BYTES[ref.vt]);
    if (!ref || !n || !waiting.has(ref) || lo == null || hi == null) continue;
    // kind: 0 u8, 1 s8, 2 u16, 3 s16, 4 bits of a byte, 5 bits of a word
    const word = lo | hi << 8;
    const m = kind === 1 ? lo << 24 >> 24 : kind === 2 ? word : kind === 3 ? word << 16 >> 16 : lo;
    const wire = kind === 4 ? (lo >> a) & b : kind === 5 ? (word >> a) & b : (a * m + b) & (2 ** (7 * n) - 1);
    store(ref, decode(ref, Array.from({ length: n }, (_, i) => (wire >> 7 * i) & 127)));
    peekedRefs.push(ref);
  }
  const have = new Set(peekedRefs), every = Math.ceil(peekedRefs.length / 12);
  const checks = peekedRefs.filter((_, i) => i % every === 0);
  for (const ref of checks) spot.set(ref.key, vals.get(ref.key));
  queue = [...checks, ...queue.filter(r => !have.has(r))];
  done = total - queue.length;
  pump();
}
let queue = [], done = 0, total = 0;
const inflight = new Map();
let patchReadWaiters = [];
const disableVelocityPending = new Map();
const velocityBackups = new Map();
try {
  const saved = JSON.parse(localStorage.getItem('xwp1.velocity-backups') || '{}');
  for (const [key, values] of Object.entries(saved)) {
    if (values && typeof values === 'object' && !Array.isArray(values)) {
      const backup = new Map(Object.entries(values).filter(([, value]) => Number.isFinite(value)));
      if (backup.size) velocityBackups.set(key, backup);
    }
  }
} catch (e) { /* storage is optional */ }
function persistVelocityBackups() {
  try {
    localStorage.setItem('xwp1.velocity-backups', JSON.stringify(Object.fromEntries(
      [...velocityBackups].map(([key, values]) => [key, Object.fromEntries(values)]))));
  } catch (e) { /* the toggles still work for this visit */ }
}
function velocityContext() {
  const storedTone = vals.get(toneNumber().key);
  const tone = activeEngine === 'hex' ? window.hexProgram ?? storedTone
    : activeEngine === 'draw' ? window.drawProgram ?? storedTone
      : activeEngine === 'pcm' ? window.pcmProgram ?? storedTone : storedTone;
  return { engine: activeEngine, tone, key: tone == null ? null : `${activeEngine}:${tone}` };
}
const velocityBackupKey = (context, group) => context.key == null ? null : `${context.key}/${group}`;
function velocityTargets(group, engine) {
  return [...refs.values()].filter(ref => {
    if (engine === 'solo') return ref.ct === 9 && (group === 'amp'
      ? ref.id === 'tssOSCAtch' : group === 'filter' && (ref.id === 'tssOSCFtch' || ref.id === 'tssFLTFtch'));
    if (group !== 'amp') return false;
    if (engine === 'hex') return ref.ct === 8 && ref.id === 'hexTouchSenseOfs';
    if (engine === 'pcm') return ref.id === 'pcmTouchSense';
    return false;
  });
}
function velocityNeutral(ref) { return ref.id === 'hexTouchSenseOfs' ? 128 : 0; }
function setVelocityOff(group, disabled, context = velocityContext()) {
  const key = velocityBackupKey(context, group);
  if (!key) return false;
  const targets = velocityTargets(group, context.engine).filter(ref => vals.get(ref.key) != null);
  if (!targets.length) return false;
  let backup = velocityBackups.get(key);
  if (disabled) {
    if (!backup) {
      backup = new Map(targets.map(ref => [ref.key, vals.get(ref.key)]));
      velocityBackups.set(key, backup);
      persistVelocityBackups();
    }
    const updates = targets.map(ref => ({ ref, value: velocityNeutral(ref) }));
    const applied = context.engine === 'solo' && window.macros?.setVelocityValues?.(context.engine, updates);
    if (!applied) updates.forEach(({ ref, value }) => editLater(ref, value));
  } else {
    if (!backup) return false;
    const updates = targets.filter(ref => backup.has(ref.key)).map(ref => ({ ref, value: backup.get(ref.key) }));
    const applied = context.engine === 'solo' && window.macros?.setVelocityValues?.(context.engine, updates);
    if (!applied) updates.forEach(({ ref, value }) => editLater(ref, value));
    velocityBackups.delete(key);
    persistVelocityBackups();
  }
  return true;
}
function updateVelocityButtons() {
  const context = velocityContext();
  for (const group of ['amp', 'filter']) {
    const button = $(`#disable${group[0].toUpperCase()}${group.slice(1)}Velocity`);
    if (!button) continue;
    if (button.velocityNotice) continue;
    const pending = disableVelocityPending.get(group);
    const pendingMatches = pending && pending.engine === context.engine && (pending.tone == null || pending.tone === context.tone);
    const disabled = pendingMatches ? pending.disabled : velocityBackupKey(context, group) != null && velocityBackups.has(velocityBackupKey(context, group));
    button.textContent = `${group[0].toUpperCase()}${group.slice(1)} vel ${disabled ? 'off' : 'on'}`;
    button.title = disabled ? `Restore ${group} touch sensitivity values saved for this tone` : `Set available ${group} touch sensitivity to zero; click again to restore the saved values`;
    button.classList.toggle('on', !disabled);
    button.setAttribute('aria-pressed', String(!disabled));
  }
}
window.updateVelocityButtons = updateVelocityButtons;
function readPatch(withTone, onReady = null) {
  if (onReady) patchReadWaiters.push(onReady);
  const engineCt = activeEngine === 'solo' ? 9 : activeEngine === 'hex' ? 8 : 7;
  const pcm = activeEngine === 'pcm';
  inflight.clear();
  for (const m of [later, written, checking]) m.clear();
  for (const ref of refs.values()) if (withTone || ref !== toneNumber()) forget(ref);
  queue = [...refs.values()].filter(r => (withTone || r !== toneNumber()) &&
    (pcm ? (!([7, 8, 9].includes(r.ct)) && (r.id?.startsWith('pcm') || ![3, 5].includes(r.ct)))
      : (r.ct !== 7 && r.ct !== 8 && r.ct !== 9 || r.ct === engineCt)));
  // what is on screen first: the tone, mixer and effect, the switches and waves, the selected block, then the rest
  const rank = r => pcm ? (r.id?.startsWith('pcm') ? 1 : 0) : r.ct !== engineCt ? 0 : activeEngine === 'solo'
    ? ((r.id === 'tssOSCsw' || r.id === 'tssOSCwf' || r.id === 'tssOSCAlvl') ? 1 : (P[r.id].count < 6 || r.inst === selected) ? 2 : 3 + r.inst)
    : activeEngine === 'hex'
      ? (r.id === 'hexOnoff' || r.id === 'hexWaveNumber' || r.id === 'hexVolumeOfs') ? 1
        : (r.count === 1 || r.inst === window.hexSelectedLayer) ? 2 : 3 + r.inst
      : r.id === 'organPosition' ? 1 : 2;
  queue.sort((a, b) => rank(a) - rank(b));
  done = 0;
  total = queue.length;
  $('#loadbar').classList.remove('done');
  clearTimeout(peeking?.timer);
  peeking = null;
  spot.clear();
  const mappedCells = activeEngine === 'solo' ? cells : otherCells[activeEngine]
    ? [...(cells || []).filter(c => c[0] !== 9), ...otherCells[activeEngine]] : null;
  const wanted = new Set(queue.map(r => r.key));
  peekCells = (mappedCells || []).filter(c => wanted.has(keyOf(...c.slice(0, 4))));
  peekSpans = spansOf(peekCells);
  if (peekSpans.length) peek(); else pump();
}
function pump() {
  if (peeking) return;
  while (inflight.size < 6 && queue.length) {
    const ref = queue.shift();
    inflight.set(ref.key, { ref, at: performance.now(), tries: 1 });
    sendMidi(requestMessage(ref));
  }
  $('#loadbar i').style.width = (total ? Math.min(100, 100 * done / total) : 0) + '%';      // answers can outnumber the first count
  if (!inflight.size && !queue.length) {
    const finished = !$('#loadbar').classList.contains('done');
    $('#loadbar').classList.add('done');
    if (finished && window.macros) window.macros.syncLfo();
    if (finished) {
      const context = velocityContext();
      for (const group of ['amp', 'filter']) {
        const key = velocityBackupKey(context, group);
        if (key && velocityBackups.has(key)) setVelocityOff(group, true, context);
      }
      for (const [group, pending] of disableVelocityPending) {
        const matches = pending.engine === context.engine && (pending.tone == null || pending.tone === context.tone);
        if (matches) {
          const key = velocityBackupKey(context, group);
          if ((pending.disabled || key && velocityBackups.has(key)) && !setVelocityOff(group, pending.disabled, context)) showVelocityUnavailable(group);
        }
        disableVelocityPending.delete(group);
      }
      updateVelocityButtons();
    }
    for (const ready of patchReadWaiters.splice(0)) ready();
  }
}
setInterval(() => {
  const now = performance.now();
  for (const [key, f] of inflight) {
    if (now - f.at < 400) continue;
    if (f.tries >= 6) { inflight.delete(key); done++; continue; }
    f.tries++;
    f.at = now;
    sendMidi(requestMessage(f.ref));
  }
  if (inflight.size || queue.length) pump();
}, 50);

// ---------------------------------------------------------------- formatting

function show(ref, v) {
  if (v == null) return '–';
  if (ref.vt === 'hxwf') { const i = v === 0 ? 0 : v - 326; return `${String(i).padStart(3, '0')}  ${window.hexData.waves[i] || 'Unknown wave'}`; }
  if (ref.steps) return nearest(ref.steps, v)[1];
  if (ref.labels) return ref.labels[v] ?? String(v);
  if (ref.vt === 'pk') return (v >= 0 ? '+' : '') + (v / 512).toFixed(2) + ' st';
  if (ref.ct === 2 && ref.pid === D.mixer.pan) return v === 64 ? 'C' : v < 64 ? `L${64 - v}` : `R${v - 64}`;
  if (ref.hexCenter != null) { const s = v - ref.hexCenter; return (s > 0 ? '+' : '') + s; }
  if (ref.min < 0 || ref.bipolar) { const s = ref.bipolar ? v - 64 : v; return (s > 0 ? '+' : '') + s; }
  return String(v);
}
const nearest = (steps, v) => steps.reduce((a, b) => Math.abs(b[0] - v) < Math.abs(a[0] - v) ? b : a);
const NOTE = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
const noteName = n => NOTE[n % 12] + (Math.floor(n / 12) - 1);
const NOTES = Array.from({ length: 128 }, (_, n) => noteName(n));

// ---------------------------------------------------------------- controls

const tip = $('#tip');
function showTip(x, y, text) { tip.hidden = false; tip.textContent = text; tip.style.left = x + 'px'; tip.style.top = y + 'px'; }
const hideTip = () => { tip.hidden = true; };

function arc(cx, cy, r, a0, a1) {
  const p = a => [cx + r * Math.cos(a), cy + r * Math.sin(a)];
  const [x0, y0] = p(a0), [x1, y1] = p(a1);
  return `M${x0.toFixed(2)} ${y0.toFixed(2)}A${r} ${r} 0 ${Math.abs(a1 - a0) > Math.PI ? 1 : 0} ${a1 > a0 ? 1 : 0} ${x1.toFixed(2)} ${y1.toFixed(2)}`;
}
const A0 = Math.PI * 0.75, A1 = Math.PI * 2.25;

// A knob for one parameter. opts: label, size ('big' | 'sm'), note (show as a note name).
function knob(ref, opts = {}) {
  const label = opts.label ?? ref.name;
  const value = el('path', { class: 'value' }), tick = el('line', { class: 'tick' });
  const svg = el('svg', { class: 'knob', viewBox: '0 0 44 44', tabindex: 0, role: 'slider', 'aria-label': ref.name },
    el('path', { class: 'track', d: arc(22, 22, 18, A0, A1) }), value,
    el('circle', { class: 'cap', cx: 22, cy: 22, r: 12.5 }), tick);
  const out = el('output');
  const box = el('div', { class: `ctl wait ${opts.size || ''}`, title: ref.name }, el('label', { text: label }), svg, out);
  const text = v => opts.text ? opts.text(v) : opts.note ? (v == null ? '–' : noteName(v)) : show(ref, v);
  const edit = opts.set || (v => window.edit(ref, v));     // a macro knob has its own way of setting
  const centre = ref.hexCenter != null ? ref.hexCenter : ref.min < 0 ? 0 : ref.bipolar ? 64 : null;
  watch(ref, v => {
    box.classList.toggle('wait', v == null);
    const f = v == null ? 0 : (v - ref.min) / (ref.max - ref.min || 1);
    const a = A0 + f * (A1 - A0);
    const from = centre == null ? A0 : A0 + (centre - ref.min) / (ref.max - ref.min) * (A1 - A0);
    value.setAttribute('d', v == null || Math.abs(a - from) < 0.01 ? '' : arc(22, 22, 18, Math.min(from, a), Math.max(from, a)));
    tick.setAttribute('x1', 22 + 5 * Math.cos(a)); tick.setAttribute('y1', 22 + 5 * Math.sin(a));
    tick.setAttribute('x2', 22 + 11 * Math.cos(a)); tick.setAttribute('y2', 22 + 11 * Math.sin(a));
    out.textContent = text(v);
    svg.setAttribute('aria-valuenow', v ?? '');
  });
  const steps = ref.steps && ref.steps.map(s => s[0]);
  const nudge = n => {
    const v = vals.get(ref.key) ?? ref.min;
    if (steps) { const i = steps.indexOf(nearest(ref.steps, v)[0]); edit(steps[clamp(i + n, 0, steps.length - 1)]); }
    else edit(v + n * (ref.step || 1));
  };
  let start = null;
  svg.addEventListener('pointerdown', e => {
    if (vals.get(ref.key) == null) return;
    svg.setPointerCapture(e.pointerId);
    start = { y: e.clientY, v: vals.get(ref.key) };
    box.classList.add('drag');
    e.preventDefault();
  });
  svg.addEventListener('pointermove', e => {
    if (!start) return;
    const range = ref.max - ref.min;
    const per = (e.shiftKey ? 0.25 : 1) * range / (ref.step > 1 ? 480 : range > 300 ? 300 : 150);
    let v = start.v + (start.y - e.clientY) * per;
    if (steps) v = nearest(ref.steps, v)[0];
    const grain = e.shiftKey ? ref.fine || 1 : ref.step || 1;
    if (grain > 1) v = Math.round(v / grain) * grain;
    edit(v);
    const r = svg.getBoundingClientRect();
    showTip(r.left + r.width / 2, r.top, text(vals.get(ref.key)));
  });
  const end = () => { start = null; box.classList.remove('drag'); hideTip(); };
  svg.addEventListener('pointerup', end);
  svg.addEventListener('pointercancel', end);
  svg.addEventListener('wheel', e => { e.preventDefault(); nudge(e.deltaY < 0 ? 1 : -1); }, { passive: false });
  svg.addEventListener('dblclick', () => edit(centre ?? opts.reset ?? vals.get(ref.key)));
  svg.addEventListener('keydown', e => {
    const n = { ArrowUp: 1, ArrowRight: 1, ArrowDown: -1, ArrowLeft: -1, PageUp: 10, PageDown: -10 }[e.key];
    if (n) { e.preventDefault(); nudge(n); }
  });
  return box;
}

// An on / off switch.
function led(ref, title, set) {
  const b = el('button', { class: 'led', title: title || ref.name, role: 'switch', 'aria-label': title || ref.name });
  watch(ref, v => { b.classList.toggle('on', v === 1); b.classList.toggle('wait', v == null); b.setAttribute('aria-checked', v === 1); });
  b.addEventListener('click', e => { e.stopPropagation(); (set || (v => edit(ref, v)))(vals.get(ref.key) === 1 ? 0 : 1); });
  return b;
}
const field = (text, control) => el('span', { class: 'field' }, control, text);

// A few named choices side by side.
function seg(ref, labels, set) {
  const box = el('div', { class: 'seg', role: 'radiogroup', 'aria-label': ref.name });
  const buttons = labels.map((text, i) => {
    if (text == null) return null;
    const b = el('button', { text, role: 'radio' });
    b.addEventListener('click', () => (set || (v => edit(ref, v)))(i));
    box.append(b);
    return b;
  });
  watch(ref, v => buttons.forEach((b, i) => b && b.classList.toggle('on', i === v)));
  return box;
}

// A named choice from a list (opens a searchable popover when it is long).
function pick(ref, labels, opts = {}) {
  const b = el('button', { class: 'pick', title: ref.name }, opts.label ? el('small', { text: opts.label }) : null, el('b'));
  watch(ref, v => { $('b', b).textContent = v == null ? '–' : labels[v - (opts.base || 0)] ?? String(v); });
  b.addEventListener('click', e => {
    e.stopPropagation();
    chooser(b, labels, vals.get(ref.key) - (opts.base || 0), i => (opts.set || (v => edit(ref, v)))(i + (opts.base || 0)), opts);
  });
  return b;
}

const popover = $('#popover');
function closePopover() { popover.hidden = true; popover.replaceChildren(); midiOpen = false; instancesOpen = false; }
document.addEventListener('pointerdown', e => { if (!popover.hidden && !popover.contains(e.target)) closePopover(); });
document.addEventListener('keydown', e => { if (e.key === 'Escape') closePopover(); });

function chooser(anchor, labels, current, choose, opts = {}) {
  const LIMIT = 300;
  const list = el('div', { class: 'list' + (opts.cols || labels.length > 24 ? ' cols' : '') });
  // opts.wave(i) -> [block, wave]: the picture of the wave under the pointer, or of the chosen one
  let shown = null;
  const peek = opts.wave && wavePics.index ? wavePicture(() => opts.wave(shown ?? current), 'big') : null;
  if (peek) list.addEventListener('pointerleave', () => { shown = null; peek.draw(); });
  const search = labels.length > 12 ? el('input', { class: 'search', placeholder: `Search ${labels.length} ${opts.what || 'items'}`, type: 'search' }) : null;
  const fill = () => {
    const q = search ? search.value.trim().toLowerCase() : '';
    const hits = [];
    for (let i = 0; i < labels.length && hits.length <= LIMIT; i++) if (!q || labels[i].toLowerCase().includes(q) || String(i + (opts.from ?? 0)).startsWith(q)) hits.push(i);
    list.replaceChildren(...hits.slice(0, LIMIT).map(i => {
      const item = el('button', { class: 'item' + (i === current ? ' sel' : '') }, el('small', { text: opts.number === false ? '' : i + (opts.from ?? 0) }), labels[i]);
      item.addEventListener('click', () => { choose(i); if (!opts.keep) closePopover(); else { current = i; fill(); } });
      if (peek) item.addEventListener('pointerenter', () => { shown = i; peek.draw(); });
      return item;
    }));
    if (hits.length > LIMIT) list.append(el('div', { class: 'more', text: 'Type to narrow the list…' }));
    list.scrollTop = 0;
  };
  if (search) search.addEventListener('input', fill);
  fill();
  popover.replaceChildren(...[search, peek && peek.wrap, list].filter(Boolean));
  popover.hidden = false;
  const r = anchor.getBoundingClientRect();
  popover.style.width = opts.width ? `${opts.width}px` : '';
  const w = popover.offsetWidth, h = popover.offsetHeight;
  popover.style.left = clamp(r.left, 12, innerWidth - w - 12) + 'px';
  popover.style.top = (r.bottom + h + 8 > innerHeight ? Math.max(12, r.top - h - 6) : r.bottom + 6) + 'px';
  const sel = $('.sel', list);
  if (sel) sel.scrollIntoView({ block: 'center' });
  if (search && matchMedia('(pointer: fine)').matches) search.focus();
}

// ---------------------------------------------------------------- graphs

// A graph is drawn in its own pixels: `draw` runs again whenever its size changes.
const graphs = new ResizeObserver(entries => { for (const e of entries) if (e.target.redraw) e.target.redraw(); });
function graph(cls, cap, low) {
  const svg = el('svg', { class: 'graph ' + cls });
  graphs.observe(svg);
  svg.size = () => {
    const w = Math.max(svg.clientWidth, 60), h = Math.max(svg.clientHeight, 40);
    svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
    return [w, h];
  };
  return { svg, wrap: el('div', { class: 'gwrap low' + (low ? '' : ' right') }, svg, cap ? el('span', { class: 'cap', text: cap }) : null) };
}

// A seven-stage envelope, drawn and edited by dragging its corners:
// init level, attack time / level, decay time / sustain level, then after
// the key is released two more times and levels. In the ADSR view (macro.js)
// only attack time, decay / sustain and release time have corners, and the
// first drag sets the other values to an ADSR's.
function envelope(prefix, inst, cap, cls = '') {
  const r = s => solo(prefix + s, inst);
  const R = { iL: r('iL'), aT: r('aT'), aL: r('aL'), dT: r('dT'), sL: r('sL'), r1T: r('r1T'), r1L: r('r1L'), r2T: r('r2T'), r2L: r('r2L') };
  const { svg, wrap } = graph(cls, cap);
  const lo = R.aL.min, hi = R.aL.max, X0 = 12, PAD = 17;
  let W = 300, H = 100, SEG = 56, HOLD = 34;
  const lane = { tssOSCPENV: 'pitch', tssOSCFENV: 'filter', tssOSCAENV: 'amp' }[prefix];
  const adsr = () => lo >= 0 && vals.get(window.macros.adsr.key) === 1;
  const tidy = () => {
    const to = [[R.iL, lo], [R.aL, hi], [R.r1L, lo], [R.r2T, 0], [R.r2L, lo]];
    if (to.some(([ref, v]) => vals.get(ref.key) != null && vals.get(ref.key) !== v)) window.macros.remember();     // Undo brings the old shape back
    for (const [ref, v] of to) edit(ref, v);
  };
  const y = v => H - PAD - (v - lo) / (hi - lo) * (H - 2 * PAD);
  const level = py => clamp(Math.round(lo + (H - PAD - py) / (H - 2 * PAD) * (hi - lo)), lo, hi);
  const get = k => vals.get(R[k].key) ?? (k.endsWith('T') ? 0 : lo < 0 ? 0 : lo);
  const fill = el('path', { class: 'fill' }), line = el('path', { class: 'line' }), hold = el('line', { class: 'line hold' });
  const zero = el('line', { class: 'grid' }), on = el('line', { class: 'grid' }), off = el('line', { class: 'grid' });
  const keyOn = el('text', { text: 'key down' }), keyOff = el('text', { text: 'key up' });
  svg.append(on, off, zero, keyOn, keyOff, fill, line, hold);
  const nodes = [['iL', null], ['aL', 'aT'], ['sL', 'dT'], null, ['r1L', 'r1T'], ['r2L', 'r2T']].map((n, at) => {
    if (!n) return null;
    const c = el('circle', { class: 'node', r: 5.5 });
    let start = null;
    c.addEventListener('pointerdown', e => {
      c.setPointerCapture(e.pointerId);
      start = { x: e.clientX, t: n[1] ? get(n[1]) : 0, top: svg.getBoundingClientRect().top, adsr: adsr() };
      if (start.adsr) tidy();
      c.classList.add('drag');
      e.preventDefault();
    });
    c.addEventListener('pointermove', e => {
      if (!start) return;
      if (!start.adsr || at === 2) edit(R[n[0]], level(e.clientY - start.top));
      if (n[1]) edit(R[n[1]], start.t + (e.clientX - start.x) / SEG * 127);
      showTip(e.clientX, e.clientY, (n[1] ? `time ${get(n[1])}   ` : '') + `level ${show(R[n[0]], get(n[0]))}`);
    });
    const end = () => { start = null; c.classList.remove('drag'); hideTip(); };
    c.addEventListener('pointerup', end);
    c.addEventListener('pointercancel', end);
    svg.append(c);
    return c;
  });
  const draw = () => {
    [W, H] = svg.size();
    HOLD = Math.min(44, W * 0.12);
    SEG = (W - 2 * X0 - HOLD) / 4;
    const seg = k => 4 + get(k) / 127 * (SEG - 4);
    const p = [[X0, y(get('iL'))]];
    p.push([p[0][0] + seg('aT'), y(get('aL'))]);
    p.push([p[1][0] + seg('dT'), y(get('sL'))]);
    p.push([X0 + 2 * SEG + HOLD, y(get('sL'))]);
    p.push([p[3][0] + seg('r1T'), y(get('r1L'))]);
    p.push([p[4][0] + seg('r2T'), y(get('r2L'))]);
    const d = pts => pts.map((q, i) => (i ? 'L' : 'M') + q[0].toFixed(1) + ' ' + q[1].toFixed(1)).join('');
    line.setAttribute('d', d(p.slice(0, 3)) + d(p.slice(3)));
    const set = (node, a) => { for (const k in a) node.setAttribute(k, a[k]); };
    set(hold, { x1: p[2][0], x2: p[3][0], y1: p[2][1], y2: p[3][1] });
    const base = y(lo < 0 ? 0 : lo);
    fill.setAttribute('d', d(p) + `L${p[5][0].toFixed(1)} ${base}L${X0} ${base}Z`);
    set(zero, { x1: 0, x2: W, y1: base, y2: base });
    set(on, { x1: X0, x2: X0, y1: 0, y2: H });
    set(off, { x1: p[3][0], x2: p[3][0], y1: 0, y2: H });
    set(keyOn, { x: X0 + 4, y: 10 });
    // the buttons in the corner (macro.js) take the room to the right of the line on a narrow graph
    const room = W - (wrap.querySelector('.gtools')?.offsetWidth || 0) - 12 - p[3][0] > 44;
    set(keyOff, { x: p[3][0] + (room ? 4 : -4), y: 10, 'text-anchor': room ? 'start' : 'end' });
    nodes.forEach((c, i) => { if (c) set(c, { cx: p[i][0], cy: p[i][1], visibility: adsr() && (i === 0 || i === 5) ? 'hidden' : 'visible' }); });
  };
  svg.redraw = draw;
  for (const ref of Object.values(R)) watch(ref, draw);
  watch(window.macros.adsr, draw);
  wrap.append(window.macros.envelopeTools(R, lane, inst, lo < 0));
  return wrap;
}

// A response curve: fn(f) -> gain in dB, redrawn when any of `deps` changes.
function curve(deps, fn, cap, cls = 'sm', range = [-24, 12]) {
  const { svg, wrap } = graph(cls, cap, true);
  const fill = el('path', { class: 'fill' }), line = el('path', { class: 'line' });
  const grid = [100, 1000, 10000].map(f => [f, el('line', { class: 'grid' }), el('text', { text: f < 1000 ? f : f / 1000 + 'k' })]);
  const zero = el('line', { class: 'grid' });
  svg.append(...grid.flatMap(g => g.slice(1)), zero, fill, line);
  const draw = () => {
    const [W, H] = svg.size();
    const x = f => Math.log(f / 20) / Math.log(1000) * W;
    const y = db => H - (clamp(db, range[0] - 6, range[1]) - range[0]) / (range[1] - range[0]) * H;
    for (const [f, ln, tx] of grid) {
      ln.setAttribute('x1', x(f)); ln.setAttribute('x2', x(f)); ln.setAttribute('y1', 0); ln.setAttribute('y2', H);
      tx.setAttribute('x', x(f) + 4); tx.setAttribute('y', 11);
    }
    zero.setAttribute('x1', 0); zero.setAttribute('x2', W); zero.setAttribute('y1', y(0)); zero.setAttribute('y2', y(0));
    let d = '';
    for (let i = 0; i <= 120; i++) { const f = 20 * 1000 ** (i / 120); d += (i ? 'L' : 'M') + x(f).toFixed(1) + ' ' + y(fn(f)).toFixed(1); }
    line.setAttribute('d', d);
    fill.setAttribute('d', d + `L${W} ${H}L0 ${H}Z`);
  };
  svg.redraw = draw;
  deps.forEach(ref => watch(ref, draw));
  return wrap;
}

// The per-oscillator filter as measured on the instrument: a second-order
// shelf (Butterworth poles at the corner, zeros at sqrt(8) times it, so the
// highs end up at 1/8), blended with the unfiltered signal by Filter Gain.
function oscFilterCurve(inst) {
  const cut = solo('tssOSCFcoff', inst), gain = solo('tssOSCFgain', inst);
  return curve([cut, gain], f => {
    const fp = D.oscFilterHz[Math.min(vals.get(cut.key) ?? 15, 7)], fz = fp * Math.SQRT2 * 2, q = Math.SQRT1_2;
    const dry = [1, 0.7071, 0.5, 0.25, 0][vals.get(gain.key) ?? 0];
    // H = (1/8) (fz^2 - f^2 + j f fz / q) / (fp^2 - f^2 + j f fp / q)
    const nr = fz * fz - f * f, ni = f * fz / q, dr = fp * fp - f * f, di = f * fp / q, den = dr * dr + di * di;
    const re = dry + (1 - dry) * (nr * dr + ni * di) / den / 8, im = (1 - dry) * (ni * dr - nr * di) / den / 8;
    return 10 * Math.log10(re * re + im * im);
  }, 'response', 'sm', [-22, 6]);
}

// The Total Filter: a two-pole state-variable filter. The cutoff scale is the
// picture's own (the firmware's is not written down anywhere).
function totalFilterCurve() {
  const type = solo('tssFLTFtype'), cut = solo('tssFLTFcoff'), res = solo('tssFLTFreso');
  return curve([type, cut, res], f => {
    const fc = 24 * 2 ** ((vals.get(cut.key) ?? 127) / 127 * 9.8), q = 0.62 + ((vals.get(res.key) ?? 0) / 127) ** 2 * 11;
    const u = f / fc, dr = 1 - u * u, di = u / q, den = dr * dr + di * di;
    const num = [1, di * di, u ** 4][vals.get(type.key) ?? 0];
    return 10 * Math.log10(num / den);
  }, 'response (schematic)', '', [-36, 20]);
}

function lfoShape(inst) {
  const wf = solo('tssLFOwf', inst), rate = solo('tssLFOrate', inst), depth = solo('tssLFOdep', inst);
  const { svg, wrap } = graph('sm', null);
  const line = el('path', { class: 'line' }), mid = el('line', { class: 'grid' });
  svg.append(mid, line);
  const random = [0.3, -0.7, 0.9, -0.2, 0.55, -0.95, 0.1, 0.75, -0.5, 0.4, -0.1, 0.85];
  const shape = [t => Math.sin(2 * Math.PI * t), t => 1 - 4 * Math.abs(((t + 0.25) % 1) - 0.5), t => 2 * (t % 1) - 1, t => 1 - 2 * (t % 1),
                 t => (t % 1) < 0.25 ? 1 : -1, t => (t % 1) < 0.5 ? 1 : -1, t => (t % 1) < 0.75 ? 1 : -1, t => random[Math.floor(t * 4) % 12]];
  const draw = () => {
    const [W, H] = svg.size();
    mid.setAttribute('x1', 0); mid.setAttribute('x2', W); mid.setAttribute('y1', H / 2); mid.setAttribute('y2', H / 2);
    const cycles = 1 + (vals.get(rate.key) ?? 64) / 127 * 5, amp = (0.15 + (vals.get(depth.key) ?? 0) / 127 * 0.7) * H / 2;
    const fn = shape[vals.get(wf.key) ?? 0] || shape[0];
    let d = '', last = null;
    for (let i = 0; i <= W; i += 1) {
      const v = H / 2 - amp * fn(i / W * cycles);
      d += (i ? (last !== null && Math.abs(v - last) > amp * 0.8 ? `L${i} ${last.toFixed(1)}L` : 'L') : 'M') + `${i} ${v.toFixed(1)}`;
      last = v;
    }
    line.setAttribute('d', d);
  };
  svg.redraw = draw;
  [wf, rate, depth].forEach(r => watch(r, draw));
  return wrap;
}

// ---------------------------------------------------------------- the panel

let selected = null;            // the block whose Pitch / Filter / Amp panels are open, if any
const group = (cls, title, hint, right) => el('div', { class: `group ${cls}` },
  el('h2', {}, el('span', { text: title }), hint ? el('em', { text: hint }) : null, right ? el('span', { class: 'right' }, right) : null));
const ctls = (...kids) => el('div', { class: 'ctls' }, kids);

function buildStrips() {
  const box = $('#strips');
  D.blocks.forEach((name, i) => {
    const sw = solo('tssOSCsw', i), wave = solo('tssOSCwf', i);
    const kind = i < 2 ? 'synth' : i < 4 ? 'pcm' : i === 5 ? 'noise' : null, names = kind && D.waves[kind];
    const waveButton = el('button', { class: 'wave', title: 'Waveform' });
    if (names) {
      watch(wave, v => { waveButton.textContent = v == null ? '–' : names[v] ?? `Wave ${v}`; });
      waveButton.addEventListener('click', e => {
        e.stopPropagation();
        chooser(waveButton, names, vals.get(wave.key), v => edit(wave, v), { what: 'waves', keep: true, wave: v => [kind, v], width: names.length > 400 ? 720 : names.length > 20 ? 560 : 0 });
      });
    } else { waveButton.textContent = 'Mic / Inst input'; waveButton.disabled = true; }
    const picture = wavePicture(() => (names && vals.get(wave.key) != null ? [kind, vals.get(wave.key)] : null));
    watch(wave, picture.draw);
    const strip = el('div', { class: 'strip', tabindex: 0, 'data-block': i },
      el('div', { class: 'head' }, led(sw, `${name} on / off`), el('b', { text: name })),
      waveButton, picture.wrap,
      el('div', { class: 'lvl' }, knob(solo('tssOSCAlvl', i), { label: 'Level', size: 'sm' })));
    watch(sw, v => strip.classList.toggle('off', v === 0));
    // turning the level knob or the switch is not a click on the block
    strip.addEventListener('click', e => { if (!e.target.closest('.ctl, .wave, .led')) select(i, true); });
    strip.addEventListener('keydown', e => { if (e.target === strip && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); select(i, true); } });
    box.append(strip);
  });
}
// Open a block's panels; clicking the open block again closes them.
function select(i, toggle = false) {
  if (i === selected) { if (!toggle) return; i = null; }
  selected = i;
  document.querySelectorAll('.strip').forEach((st, n) => { st.classList.toggle('sel', n === i); st.setAttribute('aria-expanded', n === i); });
  $('#editor').hidden = i == null;
  if (i == null) { $('#editor').replaceChildren(); return; }
  buildEditor();
  // its values first, if the tone is still being read
  queue.sort((a, b) => (b.ct === 9 && b.inst === i) - (a.ct === 9 && a.inst === i));
}

const sub = (title, ...kids) => el('div', { class: 'subrow' }, el('b', { text: title }), kids);

function buildEditor() {
  const i = selected, s = id => solo(id, i), name = D.blocks[i];
  const clock = (id, labels = D.enums.clockTrigger) => pick(s(id), labels, { label: 'Clock', number: false });
  const small = (ref, label, o = {}) => knob(ref, { label, size: 'sm', ...o });

  const pitch = group('pitch', `${name} \u00b7 Pitch`, 'tuning, glide and the pitch envelope', window.macros.blockButton(i));
  pitch.append(envelope('tssOSCPENV', i, 'pitch envelope'),
    ctls(knob(s('tssOSCPoset'), { label: 'Offset' }), knob(s('tssOSCPdtne'), { label: 'Detune' }), knob(s('tssOSCPEdep'), { label: 'Env' }),
         knob(s('tssOSCPlfo1D'), { label: 'LFO 1' }), knob(s('tssOSCPlfo2D'), { label: 'LFO 2' }), knob(s('tssOSCPkeyf'), { label: 'Key flw' }),
         knob(s('tssOSCPkeyfB'), { label: 'Flw base', note: true })),
    sub('Glide', small(s('tssOSCPortaTm'), 'Time'), field('Portamento', led(s('tssOSCPortaSw'))), field('Legato', led(s('tssOSCLegatoSw'))),
        clock('tssOSCPEclk')));
  if (i < 2) {
    pitch.append(sub('Pulse', small(solo('tssOSCPWMpw', i), 'Width'), small(solo('tssOSCPWMlfo1D', i), 'LFO 1'), small(solo('tssOSCPWMlfo2D', i), 'LFO 2'),
                     field('Sync 2 to 1', led(solo('tssOSC2sync')))));
  }

  const filt = group('filt', 'Filter', 'a treble shelf for this block alone');
  filt.append(oscFilterCurve(i), envelope('tssOSCFENV', i, 'filter envelope', 'sm'),
    el('div', { class: 'fields', style: 'margin-bottom:8px' }, seg(s('tssOSCFgain'), ['Off', '-3', '-6', '-12', '-18 dB']), clock('tssOSCFEclk')),
    ctls(knob(s('tssOSCFcoff'), { label: 'Cutoff' }), knob(s('tssOSCFEdep'), { label: 'Env' }), knob(s('tssOSCFlfo1D'), { label: 'LFO 1' }),
         knob(s('tssOSCFlfo2D'), { label: 'LFO 2' }), knob(s('tssOSCFtch'), { label: 'Touch' }), knob(s('tssOSCFkeyf'), { label: 'Key flw' }),
         knob(s('tssOSCFkeyfB'), { label: 'Flw base', note: true })),
    el('div', { class: 'note', text: 'On this synth the envelope and LFOs fade between the dry and the filtered sound; they do not move the cutoff.' }));

  const ampl = group('ampl', 'Amp', 'level and its envelope');
  ampl.append(envelope('tssOSCAENV', i, 'amp envelope'),
    ctls(knob(s('tssOSCAlvl'), { label: 'Level' }), knob(s('tssOSCAtch'), { label: 'Touch' }), knob(s('tssOSCAlfo1D'), { label: 'LFO 1' }),
         knob(s('tssOSCAlfo2D'), { label: 'LFO 2' }), knob(s('tssOSCAkeyf'), { label: 'Key flw' }), knob(s('tssOSCAkeyfB'), { label: 'Flw base', note: true })),
    el('div', { class: 'fields', style: 'margin-top:8px' }, clock('tssOSCAEclk')));

  const panels = [pitch, filt, ampl];
  if (i === 4) {
    // only the Ext block has these: the input itself, its pitch shifter, and envelopes triggered by the input's level
    const ext = group('gen', 'Input', 'the Mic / Inst jack');
    ext.append(ctls(knob(solo('tssOSCXinlvl'), { label: 'Level', size: 'big' }), knob(solo('tssOSCXokey'), { label: 'Orig key', note: true }),
                    knob(solo('tssOSCXPshmode'), { label: 'Shift mode' }), knob(solo('tssOSCXPshmix'), { label: 'Shift mix' })),
      el('div', { class: 'sep' }),
      el('div', { class: 'note', style: 'margin:0 0 8px', text: 'Let the input\u2019s level trigger these envelopes:' }),
      el('div', { class: 'fields' }, field('Pitch', led(solo('tssOSCXPxtrg'))), field('Filter', led(solo('tssOSCXFxtrg'))),
         field('Amp', led(solo('tssOSCXAxtrg'))), field('Total filter', led(solo('tssOSCXTFxtrg')))),
      ctls(knob(solo('tssOSCXngth'), { label: 'Threshold' }), knob(solo('tssOSCXngrel'), { label: 'Release' })));
    panels.unshift(ext);
  }
  $('#editor').classList.toggle('four', panels.length === 4);
  $('#editor').replaceChildren(...panels);
  // a block that is switched off: its settings stay editable, but shown at rest
  watch(s('tssOSCsw'), v => { if (selected === i) for (const g of [pitch, filt, ampl]) g.classList.toggle('asleep', v === 0); });
}

function buildTotalFilter() {
  const g = $('#tf'), s = id => solo(id);
  g.append(el('h2', {}, el('span', { text: 'Total Filter' }), el('em', { text: 'all blocks together' }),
              el('span', { class: 'right' }, seg(s('tssFLTFtype'), ['LP', 'BP', 'HP']))),
    el('div', { class: 'tfbody' },
      el('div', {}, totalFilterCurve(),
         ctls(knob(s('tssFLTFcoff'), { label: 'Cutoff', size: 'big' }), knob(s('tssFLTFreso'), { label: 'Resonance', size: 'big' }),
              knob(s('tssFLTFEdep'), { label: 'Env' }))),
      el('div', {}, envelope('tssFLTFENV', 0, 'filter envelope'),
         ctls(knob(s('tssFLTFlfo1D'), { label: 'LFO 1' }), knob(s('tssFLTFlfo2D'), { label: 'LFO 2' }), knob(s('tssFLTFtch'), { label: 'Touch' }),
              knob(s('tssFLTFkeyf'), { label: 'Key flw' })))),
    el('div', { class: 'fields', style: 'margin-top:10px' }, pick(s('tssFLTFEclk'), D.enums.clockTrigger, { label: 'Clock', number: false }),
       pick(s('tssFLTFkeyfB'), NOTES, { label: 'Follow from', number: false, cols: true, width: 420 }),
       field('Retrigger', led(s('tssFLTFErtrg')))));
}

function buildEffect() {
  const g = $('#fx'), alg = dspAlgorithm(), names = D.dsp.map(d => d.name);
  const body = el('div');
  g.append(el('h2', {}, el('span', { text: 'Effect' }), el('em', { text: 'the tone’s own' })),
           el('div', { class: 'fields', style: 'margin-bottom:10px' }, pick(alg, names, { base: 0x80, number: false })), body);
  for (let i = 0; i < 8; i++) dspParam(i);
  watch(alg, v => {
    const d = D.dsp.find(x => x.id === v);
    if (!d) { body.replaceChildren(el('div', { class: 'note', text: v == null ? '' : `Algorithm ${v} is not one of the Solo Synth effects.` })); return; }
    if (!d.params.length) { body.replaceChildren(el('div', { class: 'note', text: 'No effect: the filter’s output goes straight on.' })); return; }
    body.replaceChildren(ctls(d.params.map((p, i) => {
      const ref = dspParam(i);
      ref.steps = p.steps || null;
      ref.bipolar = !!p.bipolar;
      ref.name = `${d.name} ${p.name}`;
      return knob(ref, { label: p.name, size: i < 2 ? 'big' : '' });
    })));
    // the firmware loads the algorithm's own values when it changes
    for (let i = 0; i < 8; i++) { queue.push(dspParam(i)); total++; }
    pump();
  });
}

function buildOutput() {
  const g = $('#out');
  const type = system('reverb_type', 'Reverb type', 1);
  g.append(el('h2', {}, el('span', { text: 'Output' }), el('em', { text: 'and the system reverb' })),
    ctls(knob(mixer('volume', 'Volume'), { label: 'Volume', size: 'big' }), knob(mixer('pan', 'Pan'), { label: 'Pan' }),
         knob(mixer('chorus_send', 'Chorus send'), { label: 'Cho send' }), knob(mixer('reverb_send', 'Reverb send'), { label: 'Rev send' })),
    el('div', { class: 'sep' }),
    el('div', { class: 'fields', style: 'margin-bottom:8px' }, seg(type, ['Rectangle', 'Round'])),
    ctls(knob(system('reverb_time', 'Reverb time'), { label: 'Time' }), knob(system('reverb_level', 'Reverb level'), { label: 'Level' })));
}

function buildLfos() {
  $('#mods').replaceChildren(...[0, 1].map(i => {
    const s = id => solo(id, i);
    const g = group('lfo', `LFO ${i + 1}`, 'free-running', pick(s('tssLFOwf'), D.enums.lfoWave, { number: false }));
    g.append(el('div', { class: 'lfobody' }, lfoShape(i),
      ctls(knob(s('tssLFOrate'), { label: 'Rate', size: 'big' }), knob(s('tssLFOdep'), { label: 'Depth', size: 'big' }),
           knob(s('tssLFOdelay'), { label: 'Delay' }), knob(s('tssLFOrise'), { label: 'Rise' }), knob(s('tssLFOmdep'), { label: 'Mod whl' })),
      el('div', { class: 'fields' }, seg(s('tssLFOsync'), ['Free', i ? 'LFO 1' : null, 'Tempo']),
         pick(s('tssLFOclk'), D.enums.lfoClockTrigger, { label: 'Clock', number: false }))));
    return g;
  }));
}

// ---------------------------------------------------------------- presets

let program = null;
let activeEngine = 'solo';
window.activeEngine = activeEngine;
// The tones an editor's list offers: its presets, then the user slots of its kind (store.js): [[tone number, label]...].
function toneList(engine) {
  const presets = engine === 'hex' ? window.hexData.presets.map((name, i) => [100 + i, name])
    : engine === 'draw' ? window.drawData.presets.map((name, i) => [150 + i, name])
      : engine === 'pcm' ? window.pcmData.tones.map((name, i) => [window.pcmData.first + i, name])
        : D.presets.map((name, i) => [i, name]);
  return [...presets, ...(window.userSlots ? window.userSlots.listed(engine) : [])];
}
const listedAt = engine => toneList(engine).findIndex(([n]) => n === vals.get(toneNumber().key));
function chooseListed(engine, i) {
  const list = toneList(engine), [n] = list[((i % list.length) + list.length) % list.length];
  if (window.userSlots?.slot(n)) chooseToneNumber(n);
  else if (engine === 'hex') window.chooseHexPreset(n - 100);
  else if (engine === 'draw') window.chooseDrawPreset(n - 150);
  else if (engine === 'pcm') window.choosePcmTone(n);
  else choosePreset(n);
}
function showProgram() {
  const n = vals.get(toneNumber().key);
  program = n != null && n < D.presets.length ? n : null;
  // a user slot (what WRITE stored, or an empty one): the page of its kind, the slot and its name in the header
  const user = window.userSlots?.slot(n);
  if (user) {
    const engine = user.group.engine;
    if (engine === 'solo') { if (activeEngine !== 'solo') showSoloEngine(); }
    else if (activeEngine !== engine || { hex: window.hexProgram, draw: window.drawProgram, pcm: window.pcmProgram }[engine] != null) {
      const was = activeEngine;
      ({ hex: window.enterHex, draw: window.enterDraw, pcm: window.enterPcm })[engine](null);
      if (was !== engine) { clearTimeout(reloadTimer); reloadTimer = setTimeout(() => readPatch(false), 100); }
    }
    showUserSlot();
    updateVelocityButtons();
    return;
  }
  // Hex Layer presets are tone numbers 100..149: the instrument may already be on one (program change from outside)
  const hex = n != null && n >= 100 && n < 100 + window.hexData.presets.length ? n - 100 : null;
  const draw = n != null && n >= 150 && n < 200 ? n - 150 : null;
  const pcm = n != null && window.pcmData && n >= window.pcmData.first && n < window.pcmData.first + window.pcmData.tones.length ? n : null;
  if (pcm != null && (activeEngine !== 'pcm' || window.pcmProgram !== pcm)) {
    const was = activeEngine;
    window.enterPcm(pcm);
    if (was !== 'pcm') { clearTimeout(reloadTimer); reloadTimer = setTimeout(() => readPatch(false), 100); }
  }
  if (pcm != null) {      // the PCM page's own watcher may already have taken the number: always show it
    if (!document.body.matches('.perform-mode, .panel-mode')) {
      $('#presetNum').textContent = String(pcm).padStart(3, '0');
      $('#presetName').textContent = window.pcmData.tones[pcm - window.pcmData.first];
    }
    return;
  }
  if (draw != null && (activeEngine !== 'draw' || window.drawProgram !== draw)) {
    const was = activeEngine;
    window.enterDraw(draw);
    if (was !== 'draw') { clearTimeout(reloadTimer); reloadTimer = setTimeout(() => readPatch(false), 100); }
  }
  if (draw != null) return;
  if (hex != null && (activeEngine !== 'hex' || window.hexProgram !== hex)) {
    const was = activeEngine;
    window.enterHex(hex);
    if (was !== 'hex') { clearTimeout(reloadTimer); reloadTimer = setTimeout(() => readPatch(false), 100); }
  }
  if (hex != null) return;
  if (n == null && activeEngine !== 'solo') return;
  if (n != null && activeEngine !== 'solo') showSoloEngine();
  if (!document.body.matches('.perform-mode, .panel-mode')) {
    $('#presetNum').textContent = program == null ? '---' : String(program).padStart(3, '0');
    $('#presetName').textContent = program == null ? 'reading' : D.presets[program];
  }
  updateVelocityButtons();
}
window.showProgram = showProgram;
// The header for a user slot; the pages call it where they would show a preset's name.
function showUserSlot() {
  const user = window.userSlots?.slot(vals.get(toneNumber().key));
  if (!user || document.body.matches('.perform-mode, .panel-mode')) return !!user;
  $('#presetNum').textContent = user.label;
  $('#presetName').textContent = user.name;
  return true;
}
window.showUserSlot = showUserSlot;
function choosePreset(n, onReady = null) {
  showSoloEngine();
  n = (n + D.presets.length) % D.presets.length;
  releaseAll();
  discardToneEdits();
  sendMidi([0xB0, 0, 98, 0xB0, 0x20, 0, 0xC0, n]);
  for (const ref of refs.values()) forget(ref);      // the old tone's values are no longer true
  queue = [];
  inflight.clear();
  store(toneNumber(), n);
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(() => readPatch(!!onReady, onReady), 450);     // the firmware needs a moment to load the tone
}
function showSoloEngine() {
  window.pcmToneTarget = null;
  activeEngine = 'solo'; window.activeEngine = activeEngine;
  document.body.classList.remove('hex-mode', 'draw-mode', 'pcm-mode');
  $('#soloPage').hidden = false; $('#hexPage').hidden = true;
  $('#drawPage').hidden = true; $('#pcmPage').hidden = true;
  $('#engineFx').hidden = true; $('#fx').hidden = false;
  $('#soloTab').classList.toggle('on', !document.body.matches('.perform-mode, .panel-mode')); $('#hexTab').classList.remove('on');
  $('#drawTab').classList.remove('on'); $('#pcmTab').classList.remove('on');
  updateVelocityButtons();
}
// A tone by its number (the user slots have no bank and program of their own); the page follows from the number.
function chooseToneNumber(n, onReady = null) {
  releaseAll();
  discardToneEdits();
  window.pcmToneTarget = null;
  sendMidi(setMessage(toneNumber(), n));
  for (const ref of refs.values()) forget(ref);
  queue = [];
  inflight.clear();
  store(toneNumber(), n);
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(() => readPatch(!!onReady, onReady), 450);
}
window.chooseToneNumber = chooseToneNumber;
const chooseUserTone = chooseToneNumber;
const isSoloToneNumber = n => Number.isInteger(n) && (n >= 0 && n < D.presets.length || window.userSlots?.slot(n)?.group.id === 'solo');
window.soloPreset = {
  current: () => {
    const n = vals.get(toneNumber().key);
    return activeEngine === 'solo' && isSoloToneNumber(n) ? n : null;
  },
  label: n => n >= 0 && n < D.presets.length ? D.presets[n] : isSoloToneNumber(n) ? `${window.userSlots.slot(n).label} ${window.userSlots.slot(n).name}` : 'Unknown Solo tone',
  load: n => new Promise((resolve, reject) => {
    n = Number(n);
    if (!isSoloToneNumber(n)) { reject(new Error('The saved Solo tone number is invalid.')); return; }
    if (!linked) { reject(new Error('Connect to the synth before loading this configuration.')); return; }
    const ready = () => {
      if (window.soloPreset.current() !== n) reject(new Error('The Solo tone changed while it was loading.'));
      else resolve(n);
    };
    if (n < D.presets.length) choosePreset(n, ready);
    else chooseUserTone(n, ready);
  })
};
function buildPresets() {
  watch(toneNumber(), showProgram);
  const step = d => document.body.classList.contains('perform-mode') ? window.performanceEditor.stepPreset(d)
    : chooseListed(activeEngine, Math.max(listedAt(activeEngine), d < 0 ? 0 : -1) + d);
  $('#prevPreset').addEventListener('click', () => step(-1));
  $('#nextPreset').addEventListener('click', () => step(1));
  $('#presetButton').addEventListener('click', e => {
    e.stopPropagation();
    if (document.body.classList.contains('perform-mode')) { window.performanceEditor.openPreset(); return; }
    const engine = activeEngine, open = () => chooser($('#preset'), toneList(engine).map(([, name]) => name), listedAt(engine), i => chooseListed(engine, i),
      { what: { hex: 'Hex Layer tones', draw: 'Drawbar Organ tones', pcm: 'PCM tones' }[engine] ?? 'Solo Synth tones', cols: true, width: 720,
        from: engine === 'pcm' ? window.pcmData.first : 0 });
    open();
    // the user slots' names, should a WRITE or a Card Load on the front panel have changed them
    window.userSlots?.refresh().then(changed => { if (changed && !popover.hidden && popover.querySelector('.list')) open(); });
  });
  $('#soloTab').addEventListener('click', () => { if (activeEngine !== 'solo') choosePreset(program ?? 0); });
  $('#hexTab').addEventListener('click', () => { if (activeEngine !== 'hex') window.chooseHexPreset(window.hexProgram ?? 0); });
  $('#drawTab').addEventListener('click', () => { if (activeEngine !== 'draw') window.chooseDrawPreset(window.drawProgram ?? 0); });
  $('#pcmTab').addEventListener('click', () => { if (activeEngine !== 'pcm') window.choosePcmTone(vals.get(toneNumber().key) ?? window.pcmData.first); });
}

// ---------------------------------------------------------------- playing

const held = new Map();         // source id -> {note, channel}
const sounding = new Map();     // note -> count
let octave = 0, holding = false;
const latched = new Map();      // note -> channel
function keyboardChannel() { return poly.mode === 'multi' ? poly.channels[poly.part] - 1 : 0; }
function noteOn(source, note, velocity = 100) {
  if (note < 0 || note > 127) return;
  noteOff(source);
  const channel = keyboardChannel();
  held.set(source, { note, channel });
  if (latched.has(note)) { sendMidi([0x80 | latched.get(note), note, 0]); latched.delete(note); }
  sounding.set(note, (sounding.get(note) || 0) + 1);
  sendMidi([0x90 | channel, note, velocity]);
  paintKey(note, true);
}
function noteOff(source) {
  const voice = held.get(source);
  if (voice == null) return;
  const { note, channel } = voice;
  held.delete(source);
  const n = (sounding.get(note) || 1) - 1;
  sounding.set(note, n);
  if (n > 0) return;
  if (holding) { latched.set(note, channel); return; }
  sendMidi([0x80 | channel, note, 0]);
  paintKey(note, false);
}
function releaseAll() {
  for (const s of [...held.keys()]) noteOff(s);
  for (const [note, channel] of latched) { sendMidi([0x80 | channel, note, 0]); paintKey(note, false); }
  latched.clear();
}
function paintKey(note, down) {
  const k = $(`.key[data-note="${note}"]`);
  if (k) k.classList.toggle('down', down);
}

let firstKey = 36, keyCount = 61;
function buildKeyboard() {
  const kb = $('#keyboard');
  keyCount = innerWidth < 700 ? 25 : innerWidth < 1100 ? 37 : innerWidth < 1500 ? 49 : 61;
  firstKey = 36 + 12 * octave + (keyCount < 61 ? 12 : 0);
  kb.replaceChildren();
  const whites = [];
  for (let n = firstKey; n < firstKey + keyCount; n++) if (![1, 3, 6, 8, 10].includes(n % 12)) whites.push(n);
  whites.forEach(n => kb.append(el('div', { class: 'key' + (n % 12 === 0 ? ' c' : ''), 'data-note': n, 'data-name': noteName(n) })));
  const w = 100 / whites.length;
  for (let n = firstKey; n < firstKey + keyCount; n++) {
    if (![1, 3, 6, 8, 10].includes(n % 12)) continue;
    const left = whites.indexOf(n - 1) + 1;
    kb.append(el('div', { class: 'key black', 'data-note': n, style: `left:${(left * w - w * 0.31).toFixed(3)}%;width:${(w * 0.62).toFixed(3)}%` }));
  }
  $('#octLabel').textContent = noteName(firstKey);
  for (const n of [...sounding.keys(), ...latched.keys()]) if (sounding.get(n) > 0 || latched.has(n)) paintKey(n, true);
  dispatchEvent(new Event('keyboard-range-change'));
}
function keyboardEvents() {
  const kb = $('#keyboard');
  const at = e => {
    const k = document.elementFromPoint(e.clientX, e.clientY);
    if (!k || !k.classList.contains('key')) return null;
    const r = k.getBoundingClientRect();
    return [+k.dataset.note, clamp(Math.round(35 + 92 * (e.clientY - r.top) / r.height), 1, 127)];
  };
  kb.addEventListener('pointerdown', e => { kb.setPointerCapture(e.pointerId); const k = at(e); if (k) noteOn('p' + e.pointerId, ...k); e.preventDefault(); });
  kb.addEventListener('pointermove', e => {
    if (!held.has('p' + e.pointerId) && !(e.buttons & 1)) return;
    const k = at(e);
    if (k && held.get('p' + e.pointerId)?.note !== k[0]) noteOn('p' + e.pointerId, ...k);
  });
  const up = e => noteOff('p' + e.pointerId);
  kb.addEventListener('pointerup', up);
  kb.addEventListener('pointercancel', up);

  const map = 'awsedftgyhujkolp;';
  document.addEventListener('keydown', e => {
    if (e.repeat || e.ctrlKey || e.metaKey || e.altKey || /INPUT|TEXTAREA/.test(document.activeElement.tagName)) return;
    const i = map.indexOf(e.key.toLowerCase());
    if (i >= 0) noteOn('k' + e.code, 60 + 12 * octave + i, 100);
    else if (e.key === 'z') shiftOctave(-1);
    else if (e.key === 'x') shiftOctave(1);
  });
  document.addEventListener('keyup', e => noteOff('k' + e.code));
  addEventListener('blur', releaseAll);

  $('#octDown').addEventListener('click', () => shiftOctave(-1));
  $('#octUp').addEventListener('click', () => shiftOctave(1));
  $('#hold').addEventListener('click', e => {
    holding = !holding;
    e.currentTarget.classList.toggle('on', holding);
    if (!holding) releaseAll();
  });
  $('#panic').addEventListener('click', () => {
    releaseAll();
    for (let part = 0; part < (poly.mode === 'multi' ? 16 : 1); part++) {
      sendMidi([0xB0 | part, 123, 0]);
      sendMidi([0xB0 | part, 120, 0]);
    }
  });
  let resize = 0;
  addEventListener('resize', () => { clearTimeout(resize); resize = setTimeout(buildKeyboard, 150); });
}
function shiftOctave(d) { octave = clamp(octave + d, -2, 2); releaseAll(); buildKeyboard(); }

function wheel(node, springs, send) {
  let active = false;
  const set = f => { node.style.setProperty('--v', f); send(f); };
  const move = e => { const r = node.getBoundingClientRect(); set(clamp(1 - (e.clientY - r.top - 8) / (r.height - 16), 0, 1)); };
  node.addEventListener('pointerdown', e => { node.setPointerCapture(e.pointerId); active = true; move(e); e.preventDefault(); });
  node.addEventListener('pointermove', e => { if (active) move(e); });
  const up = () => { active = false; if (springs) set(0.5); };
  node.addEventListener('pointerup', up);
  node.addEventListener('pointercancel', up);
  node.style.setProperty('--v', springs ? 0.5 : 0);
}

// ---------------------------------------------------------------- status and sound

function status(s) {
  $('#cpu').textContent = Math.round(100 * s.cpu) + '%';
  const scale = p => clamp((20 * Math.log10(p + 1e-6) + 54) / 54, 0, 1);
  $('#meterL').style.setProperty('--v', scale(s.peak[0]));
  $('#meterR').style.setProperty('--v', scale(s.peak[1]));
  $('#cable').hidden = !s.uncabled || listening;
  if (s.voices != null && (s.voices !== poly.running || s.starting !== poly.starting)) {
    Object.assign(poly, { running: s.voices, starting: s.starting });
    polyShown();
  }
}

// The sound, in this page: each block is scheduled ahead of the audio clock
// with a small cushion (the same scheme as elektremu-studio's hub).
let listening = false, actx = null;
const stream = { rate: 0, playAt: 0 };
const CUSHION = 0.09, SLACK = 0.12;
function play(data) {
  if (!listening || !actx || !stream.rate) return;
  const pcm = new Int16Array(data.slice(1)), n = pcm.length >> 1;
  const buf = actx.createBuffer(2, n, stream.rate), L = buf.getChannelData(0), R = buf.getChannelData(1);
  for (let i = 0, j = 0; i < n; i++, j += 2) { L[i] = pcm[j] / 32768; R[i] = pcm[j + 1] / 32768; }
  const now = actx.currentTime;
  if (stream.playAt < now + 0.005) stream.playAt = now + CUSHION;
  else if (stream.playAt > now + CUSHION + SLACK) return;
  const src = actx.createBufferSource();
  src.buffer = buf;
  src.connect(actx.destination);
  src.start(stream.playAt);
  stream.playAt += n / stream.rate;
}
function listenButton() {
  $('#sound').addEventListener('click', e => {
    listening = !listening;
    e.currentTarget.classList.toggle('on', listening);
    if (listening) {
      actx = actx || new (window.AudioContext || window.webkitAudioContext)();
      actx.resume();
    }
    if (linked) ws.send(listening ? 'a 1' : 'a 0');
  });
}

// MIDI controllers: the emulator itself listens to the ones switched on here
// (and keeps doing so the next time it starts), so nothing has to be cabled.
let midiList = [], midiOpen = false, instancesOpen = false;
// The emulator can use its instances as allocated polyphonic voices, or as
// eight independent MIDI parts. This is remembered by the emulator.
const poly = { voices: 1, mode: 'poly', part: 0, bend: 48, mpe: true, glide: false, vary: [0, 0, 0, 0], channels: [1, 2, 3, 4, 5, 6, 7, 8], running: 1, starting: 0 };
let keysOn = false;             // notes play the instrument's own keyboard (zones, arpeggio, phrases): "k1"
let keysEach = false;           // "k2": with several voices each has a keyboard of its own, so an arpeggio of its own
const keyWatch = [];            // called when either changes
function polyphony([, voices, bend, mpe, glide, ...rest]) {
  const vary = rest.slice(0, 4);
  const mode = rest[4];
  const channels = rest.slice(5, 13).map(Number);
  Object.assign(poly, { voices: Number(voices), mode: mode === 'multi' ? 'multi' : 'poly', bend: Number(bend), mpe: mpe === '1', glide: glide === '1' });
  if (channels.length === 8 && channels.every(n => Number.isInteger(n) && n >= 1 && n <= 16)) poly.channels = channels;
  if (vary.length >= 4) poly.vary = vary.slice(0, 4).map(Number);  // voice variation (macro.js); a player without it sends none
  polyShown();
  for (const fn of keyWatch) fn();
}
// Polyphonic, the emulator switches the tone's portamento off unless Glide is on. Putting it back means
// loading the tone again, which takes it some seconds; either way the values here are read again.
let glideOff = null;
function polyShown() {
  const off = poly.mode === 'poly' && poly.running > 1 && !poly.glide;
  if (glideOff != null && off !== glideOff && activeEngine === 'solo') {
    clearTimeout(reloadTimer);
    reloadTimer = setTimeout(() => readPatch(false), off ? 800 : 5000);
  }
  glideOff = off;
  const instances = $('#instances');
  instances.classList.toggle('busy', poly.starting > 0);
  instances.replaceChildren(poly.mode === 'multi' ? 'Multi' : poly.voices > 1 ? 'Poly' : 'Mono',
    el('b', { text: poly.mode === 'multi' ? `${poly.part + 1}/8` : String(poly.voices) }));
  if (instancesOpen && !popover.hidden && document.activeElement?.tagName !== 'INPUT') instancesPopover();
}
function selectEditorPart(part) {
  part = clamp(Math.round(part), 0, 7);
  if (poly.part === part) return;
  releaseAll();
  poly.part = part;
  // This private message is deliberately in the same MIDI stream as the
  // requests below, so the engine changes instance before it handles them.
  sendMidi([0xF0, 0x7D, 0x58, 0x54, part, 0xF7]);
  polyShown();
  readPatch(true);
}
function polyRows() {
  const multi = poly.mode === 'multi';
  const mode = (name, label, note) => {
    const b = el('button', { class: 'pill' + (poly.mode === name ? ' on' : ''), text: label, 'aria-pressed': poly.mode === name });
    b.addEventListener('click', e => { e.stopPropagation(); ws.send('b mode ' + name); });
    return [b, el('span', { class: 'note', text: note })];
  };
  const step = (label, by) => {
    const b = el('button', { class: 'stepper', text: label, 'aria-label': by > 0 ? 'One more voice' : 'One voice fewer' });
    b.addEventListener('click', e => { e.stopPropagation(); ws.send('b voices ' + clamp(poly.voices + by, 1, 8)); });
    return b;
  };
  const state = poly.starting ? `${poly.running} playing, ${poly.starting} starting…`
    : poly.voices > 1 ? 'notes at once' : 'the instrument as it is: one note';
  const rows = [el('div', { class: 'head', text: 'Instance mode' }),
    el('div', { class: 'row' }, ...mode('poly', 'Polyphonic', 'allocate the instances so one MIDI keyboard can play chords')),
    el('div', { class: 'row' }, ...mode('multi', '8-part multi', 'eight independent parts with assignable MIDI receive channels'))];
  if (multi) {
    const parts = el('div', { class: 'part-buttons', role: 'group', 'aria-label': 'Part shown in editor' },
      Array.from({ length: 8 }, (_, part) => {
        const b = el('button', { class: 'stepper' + (part === poly.part ? ' on' : ''), text: String(part + 1), 'aria-pressed': part === poly.part, 'aria-label': `Edit part ${part + 1}`, disabled: poly.running < 8 });
        b.addEventListener('click', e => { e.stopPropagation(); selectEditorPart(part); });
        return b;
      }));
    rows.push(el('div', { class: 'row' }, 'Edit part', parts),
      el('div', { class: 'head', text: 'MIDI receive channels' }));
    for (let part = 0; part < 8; part++) {
      const channel = el('select', { 'aria-label': `Part ${part + 1} MIDI receive channel` },
        Array.from({ length: 16 }, (_, i) => el('option', { value: i + 1, text: String(i + 1) })));
      channel.value = String(poly.channels[part]);
      channel.addEventListener('click', e => e.stopPropagation());
      channel.addEventListener('change', () => { releaseAll(); ws.send(`b channel ${part + 1} ${channel.value}`); });
      rows.push(el('div', { class: 'row channel-row' }, `Part ${part + 1}`, channel));
    }
    rows.push(el('div', { class: 'row' }, el('span', { class: 'note', text: 'Parts can share a channel to play together.' })));
    return rows;
  }
  rows.push(el('div', { class: 'head', text: 'Voices' }),
    el('div', { class: 'row' }, step('−', -1), el('b', { class: 'count', text: String(poly.voices) }), step('+', 1), el('span', { class: 'note', text: state })));
  if (poly.voices > 1) {
    const mpe = el('button', { class: 'led' + (poly.mpe ? ' on' : ''), role: 'switch', 'aria-checked': poly.mpe, 'aria-label': 'MPE' });
    mpe.addEventListener('click', e => { e.stopPropagation(); ws.send('b mpe ' + (poly.mpe ? 0 : 1)); });
    const bend = el('input', { type: 'number', min: 0, max: 96, step: 1, value: poly.bend, 'aria-label': 'Bend range in semitones' });
    bend.addEventListener('click', e => e.stopPropagation());
    bend.addEventListener('change', () => { if (bend.value !== '') ws.send('b bend ' + clamp(Number(bend.value), 0, 96)); });
    const glide = el('button', { class: 'led' + (poly.glide ? ' on' : ''), role: 'switch', 'aria-checked': poly.glide, 'aria-label': 'Glide' });
    glide.addEventListener('click', e => { e.stopPropagation(); ws.send('b glide ' + (poly.glide ? 0 : 1)); });
    rows.push(el('div', { class: 'row' }, mpe, 'MPE', el('span', { class: 'note', text: poly.mpe ? 'each note has its own channel, bend and pressure' : 'an ordinary keyboard: every channel plays, the wheel bends all notes' })),
      el('div', { class: 'row' }, '±', bend, el('span', { class: 'note', text: `semitones of bend ${poly.mpe ? 'per note' : 'for the pitch wheel'} (up to 24 are reached)` })),
      el('div', { class: 'row' }, glide, 'Glide', el('span', { class: 'note', text: poly.glide ? "tones keep their portamento: each voice slides from its own last note" : 'portamento is switched off when a tone is chosen' })));
  }
  // the instrument's own keyboard: zones, arpeggio and phrases apply to what is played
  const keys = el('button', { class: 'led' + (keysOn ? ' on' : ''), role: 'switch', 'aria-checked': keysOn, 'aria-label': 'Keys' });
  keys.addEventListener('click', e => { e.stopPropagation(); ws.send(keysOn ? 'k0' : 'k1'); });
  rows.push(el('div', { class: 'row' }, keys, 'Keys', el('span', { class: 'note', text: keysOn
    ? (poly.voices > 1 ? (keysEach ? 'every voice has the instrument\u2019s keys to itself: an arpeggio per voice' : 'notes play the instrument\u2019s keys: zones, arpeggio, phrases; zone 1 on all the voices') : 'notes play the instrument\u2019s keys: zones, arpeggio, phrases')
    : 'notes play the tone directly' })));
  return rows;
}
function controllers(list) {
  midiList = list;
  const n = list.filter(c => c.on && c.here).length;
  $('#midi').replaceChildren('MIDI', el('b', { text: n ? String(n) : 'off' }));
  if (midiOpen && !popover.hidden && document.activeElement?.tagName !== 'INPUT') midiPopover();
}
function midiPopover() {
  const rows = midiList.map(c => {
    const sw = el('button', { class: 'led' + (c.on ? ' on' : ''), role: 'switch', 'aria-checked': c.on, 'aria-label': c.name });
    sw.addEventListener('click', () => ws.send((c.on ? 'm- ' : 'm+ ') + c.name));
    const [client, port = ''] = c.name.split(':');
    const label = !port || port.startsWith(client) ? (port || client) : `${client} \u2013 ${port}`;
    return el('div', { class: 'row' + (c.here ? '' : ' gone') }, sw, label + (c.here ? '' : ' (not plugged in)'));
  });
  popover.replaceChildren(el('div', { class: 'head', text: 'Play from' }),
    ...(rows.length ? rows : [el('div', { class: 'row gone', text: 'No MIDI keyboards or controllers found.' })]),
    el('div', { class: 'foot', text: 'Switch on a keyboard and it plays this synth, now and every time the emulator starts. Other programs can also send to the port "XW-P1 Emulator In".' }));
  popover.hidden = false;
  popover.style.width = '';
  const r = $('#midi').getBoundingClientRect();
  popover.style.left = clamp(r.right - popover.offsetWidth, 12, innerWidth - popover.offsetWidth - 12) + 'px';
  popover.style.top = r.bottom + 8 + 'px';
}
function instancesPopover() {
  const scroll = popover.scrollTop;
  popover.replaceChildren(...polyRows(),
    el('div', { class: 'foot', text: 'Polyphonic mode allocates instances to notes. In 8-part multi mode, choose a receive channel for each part.' }));
  popover.hidden = false;
  popover.style.width = '';
  popover.scrollTop = scroll;
  const r = $('#instances').getBoundingClientRect();
  popover.style.left = clamp(r.right - popover.offsetWidth, 12, innerWidth - popover.offsetWidth - 12) + 'px';
  popover.style.top = r.bottom + 8 + 'px';
}
// Master volume: the emulator's output gain in dB (it remembers it), not a parameter of the instrument.
function showVolume(db) {
  if (document.activeElement !== $('#vol')) $('#vol').value = db;
  $('#volText').textContent = (db > 0 ? '+' : '') + Math.round(db) + ' dB';
}
function volumeSlider() {
  const set = db => { showVolume(db); if (linked) ws.send('v ' + db); };
  $('#vol').addEventListener('input', e => set(Number(e.target.value)));
  $('#vol').addEventListener('dblclick', () => { $('#vol').value = 0; set(0); });
}
function midiButton() {
  $('#midi').addEventListener('click', e => {
    e.stopPropagation();
    midiOpen = true;
    instancesOpen = false;
    if (linked) ws.send('m?');
    midiPopover();
  });
}
function instancesButton() {
  $('#instances').addEventListener('click', e => {
    e.stopPropagation();
    instancesOpen = true;
    midiOpen = false;
    instancesPopover();
  });
}
function disableVelocityButton() {
  for (const group of ['amp', 'filter']) {
    const button = $(`#disable${group[0].toUpperCase()}${group.slice(1)}Velocity`);
    button.addEventListener('click', () => {
      const context = velocityContext();
      const key = velocityBackupKey(context, group);
      const pending = disableVelocityPending.get(group);
      const pendingMatches = pending && pending.engine === context.engine && (pending.tone == null || pending.tone === context.tone);
      const disabled = pendingMatches ? pending.disabled : key != null && velocityBackups.has(key);
      const desired = !disabled;
      const patchReady = $('#loadbar').classList.contains('done') && !queue.length && !inflight.size && !peeking;
      if (!patchReady || context.key == null) {
        disableVelocityPending.set(group, { engine: context.engine, tone: context.tone, disabled: desired });
        updateVelocityButtons();
      } else {
        disableVelocityPending.delete(group);
        if (!setVelocityOff(group, desired, context)) showVelocityUnavailable(group);
        updateVelocityButtons();
      }
    });
  }
  watch(toneNumber(), updateVelocityButtons);
  updateVelocityButtons();
}
function showVelocityUnavailable(group) {
  const button = $(`#disable${group[0].toUpperCase()}${group.slice(1)}Velocity`);
  button.velocityNotice = true;
  button.textContent = `No ${group} map`;
  clearTimeout(button.velocityNoticeTimer);
  button.velocityNoticeTimer = setTimeout(() => { button.velocityNotice = false; updateVelocityButtons(); }, 1400);
}

// ---------------------------------------------------------------- start

async function start() {
  if (window.top !== window) document.body.classList.add('framed');
  D = await (await fetch('data.json')).json();
  try { mapped(await (await fetch('mem.json')).json()); } catch (e) { /* no map: every value is asked of the firmware */ }
  for (const engine of ['hex', 'draw', 'pcm']) {
    try { otherCells[engine] = (await (await fetch(`${engine}_mem.json`)).json()).cells; } catch (e) { /* use SysEx */ }
  }
  await window.initHex();
  await window.initDraw();
  await window.initPcm();
  await window.initFx();
  for (const p of D.params) P[p.id] = p;
  document.body.append(el('svg', { width: 0, height: 0, style: 'position:absolute' }, el('defs', {},
    el('radialGradient', { id: 'capFill', cx: '35%', cy: '30%', r: '80%' },
      el('stop', { offset: '0', 'stop-color': '#4a4f58' }), el('stop', { offset: '1', 'stop-color': '#1a1c20' })))));
  // everything the firmware holds for the tone, shown or not, so one read fills it all
  for (const p of D.params) for (let i = 0; i < p.count; i++) solo(p.id, i);
  toneNumber();
  buildPresets();
  buildStrips();
  buildTotalFilter();
  buildEffect();
  buildOutput();
  buildLfos();
  window.macros.build();
  buildKeyboard();
  keyboardEvents();
  wheel($('#bend'), true, f => { const v = Math.round(f * 16383); sendMidi([0xE0, v & 127, v >> 7]); });
  wheel($('#mod'), false, f => sendMidi([0xB0, 1, Math.round(f * 127)]));
  listenButton();
  volumeSlider();
  midiButton();
  instancesButton();
  disableVelocityButton();
  connect();
}
start();
