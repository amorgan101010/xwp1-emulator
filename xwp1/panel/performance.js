// The Performance editor: which Performance, tempo, the step sequence, arpeggio
// and phrase it uses (chosen by name), and its four zones drawn over the keys.
// Everything here is the firmware's own parameters (Patch, category 2; the
// Performance number is Spec, category 0x2a), read when the page is shown and
// after a Performance change; the names are asked of the firmware once
// (preset area: mem 1, parameter set = number) and kept in the browser.
// None of these refs is in `refs`: a tone load does not read them.
'use strict';

(() => {
  const refOf = (ct, pid, inst, o) => ({ key: `perf:${ct}:${pid}:${inst}`, ct, pid, inst, ai: 0, vt: 'nf', min: 0, max: 127, ...o });
  const u8 = (pid, name, max = 199) => refOf(2, pid, 0, { vt: 'u14', max, name });
  const R = {
    number: refOf(0x2A, 0, 0, { vt: 'u14', max: 199, name: 'Performance' }),
    tempo: refOf(2, 0xA0, 0, { vt: 'u14', min: 30, max: 255, name: 'Tempo' }),
    seq: u8(0xA1, 'Step sequence'), pattern: refOf(2, 0xA5, 0, { max: 7, name: 'Pattern' }),
    seqTiming: refOf(2, 0xA3, 0, { max: 1, name: 'Sequence change' }), seqShift: refOf(2, 0xA4, 0, { max: 1, name: 'Sequencer key shift' }),
    arp: u8(0xA9, 'Arpeggio'), arpHold: refOf(2, 0xAA, 0, { max: 1, name: 'Arpeggio hold' }), arpSync: refOf(2, 0xAB, 0, { max: 2, name: 'Arpeggio sync' }),
    arpShift: refOf(2, 0xA6, 0, { max: 1, name: 'Arpeggio key shift' }), arpLo: refOf(2, 0xA7, 0, { name: 'Arpeggio range low' }), arpHi: refOf(2, 0xA8, 0, { name: 'Arpeggio range high' }),
    phrase: u8(0xAD, 'Phrase'), phraseKey: refOf(2, 0xAC, 0, { max: 1, name: 'Phrase key play' }),
    phraseLo: refOf(2, 0xAE, 0, { name: 'Phrase range low' }), phraseHi: refOf(2, 0xAF, 0, { name: 'Phrase range high' }),
  };
  const ZONE = [['on', 0xB8, 1], ['lo', 0xB9, 127], ['hi', 0xBA, 127], ['oct', 0xBD, 127], ['trans', 0xBE, 127], ['k1', 0xBF, 1], ['k2', 0xC0, 1],
    ['k3', 0xC1, 1], ['k4', 0xC2, 1], ['bend', 0xC3, 1], ['wheel', 0xC4, 1], ['pedal', 0xC5, 1], ['arp', 0xC6, 1], ['phrase', 0xC7, 1]];
  // the 16 parts (the mixer): the block index is the part. Parts 1..4 are the zones'.
  const PART = [['on', 0x68, 1], ['vol', 0x6C, 127], ['pan', 0x6E, 127], ['cho', 0x6F, 127], ['rev', 0x70, 127]];
  const M = Array.from({ length: 16 }, (_, p) => {
    const part = Object.fromEntries(PART.map(([id, pid, max]) => [id, refOf(2, pid, p, { max, name: `Part ${p + 1} ${id}` })]));
    part.pan.bipolar = true;
    part.tone = refOf(2, 0x69, p, { vt: 'u14', max: 16383, name: `Part ${p + 1} tone` });
    return part;
  });
  R.chain = refOf(0x2A, 3, 0, { vt: 'u14', max: 99, name: 'Chain' });
  const K = [0, 1, 2, 3].map(k => refOf(2, 0x98, k, { name: `Controller knob ${k + 1} sends` }));
  const Z = [0, 1, 2, 3].map(z => {
    const zone = Object.fromEntries(ZONE.map(([id, pid, max]) => [id, refOf(2, pid, z, { max, name: `Zone ${z + 1} ${id}` })]));
    Object.assign(zone.oct, { min: 62, max: 66, bipolar: true }); Object.assign(zone.trans, { min: 52, max: 76, bipolar: true });
    zone.tone = M[z].tone;
    return zone;
  });
  const SETS = { edit: [...new Set([...Object.values(R), ...Z.flatMap(z => Object.values(z)), ...K])], mix: M.flatMap(m => Object.values(m)),
                 seq: [R.seq, R.pattern], panel: [R.number, R.arpHold] }; // the front panel also shows Hold's Performance setting
  let all = SETS.edit;          // what the tab shown reads
  const byKey = new Map([...SETS.edit, ...SETS.mix].map(r => [`${r.ct}:${r.pid}:${r.inst}`, r]));

  // ---- reading: a few requests at a time, each asked again if it is not answered
  const waiting = [], flying = new Map();
  const NAMES = { perf: [2, 0x96], seq: [0x26, 0], arp: [0x28, 0], phrase: [0x29, 0], chain: [0x27, 0] };
  let names = { perf: [], seq: [], arp: [], phrase: [], chain: [] };
  try { names = { ...names, ...JSON.parse(localStorage.getItem('xwp1:names:1') || '{}'), chain: [] }; } catch (e) { /* a private window: ask again */ }
  const addr = (ct, pid, inst, mem, set, len) => [ct, mem, set & 127, set >> 7, 0, 0, 0, 0, 0, 0, inst, 0, pid & 127, pid >> 7, 0, 0, len, 0];
  function pump() {
    while (linked && flying.size < 3 && waiting.length) {
      const job = waiting.shift();
      if (flying.has(job.id)) continue;
      job.timer = setTimeout(() => { flying.delete(job.id); if (++job.tries < 5) waiting.unshift(job); pump(); }, 400);
      flying.set(job.id, job);
      sendMidi([0xF0, 0x44, 0x16, 0x03, 0x7F, 0, ...job.addr, 0xF7]);
    }
  }
  const valueJob = ref => ({ id: `${ref.ct}:0:0:${ref.pid}:${ref.inst}`, addr: addr(ref.ct, ref.pid, ref.inst, 0, 0, 0), tries: 0 });
  const read = ref => { waiting.push(valueJob(ref)); };
  // ---- the fast way: perf_mem.json (tools/perf_map.py) says in which memory cell the firmware keeps each value and
  // where in flash each preset name is, and the emulator is asked for that memory: one request, answered at once and
  // not on the instrument's MIDI OUT. Without the file, or a player that does not answer, every value is asked for.
  let cellOf = null, span = null, nameAt = null, nameOf = new Map(), peeking = 0, refresher = 0;
  const seven = (v, n) => Array.from({ length: n }, (_, i) => Math.floor(v / 128 ** i) % 128);
  fetch('perf_mem.json').then(r => r.ok ? r.json() : null).then(data => {
    if (!data) return;
    cellOf = new Map(data.cells.map(([ct, pid, inst, , at, kind, a, b]) => [`${ct}:${pid}:${inst}`, [at, kind, a, b]]));
    const at = data.cells.map(c => c[4]);
    span = [Math.min(...at), Math.max(...at) + 2];
    nameAt = data.names;
    if (shown) { readAll(); wantNames(); }
  }).catch(() => {});
  function peek() {
    clearTimeout(peeking);
    peeking = setTimeout(() => { peeking = 0; cellOf = null; readAll(); }, 700);      // no answer: the slow way from now on
    sendMidi([0xF0, 0x7D, 0x58, 0x50, ...seven(span[0], 5), ...seven(span[1] - span[0], 2), 0xF7]);
  }
  function peeked(mem) {
    clearTimeout(peeking); peeking = 0;
    const at = (addr, n) => mem[addr - span[0] + n];
    for (const ref of all) {
      const cell = cellOf.get(`${ref.ct}:${ref.pid}:${ref.inst}`);
      if (!cell || wanted.has(ref.key)) continue;           // an edit of this one is under way
      const [addr, kind, a, b] = cell, lo = at(addr, 0), word = lo | at(addr, 1) << 8, n = ref.vt === 'u14' ? 2 : 1;
      // kind: 0 u8, 1 s8, 2 u16, 3 s16, 4 bits of a byte, 5 bits of a word
      const m = kind === 1 ? lo << 24 >> 24 : kind === 2 ? word : kind === 3 ? word << 16 >> 16 : lo;
      const wire = kind === 4 ? (lo >> a) & b : kind === 5 ? (word >> a) & b : (a * m + b) & (2 ** (7 * n) - 1);
      const value = decode(ref, Array.from({ length: n }, (_, i) => (wire >> 7 * i) & 127));
      if (chosen && chosen.ref === ref) {
        // the choice is seen in the instrument: now and then it does not take one, and it is sent again
        if (value !== chosen.n) { if (chosen.tries++ < 3) select(ref, chosen.n); else { chosen = null; store(ref, value); } continue; }
        chosen = null;
      }
      store(ref, value);
    }
  }
  function readAll() {
    if (cellOf && linked) {
      // what is on the page stays there until the new values are in
      for (const ref of all) if (!cellOf.has(`${ref.ct}:${ref.pid}:${ref.inst}`)) { forget(ref); waiting.unshift(valueJob(ref)); }
      peek(); pump();
      return;
    }
    for (const w of wanted.values()) clearTimeout(w.timer);
    wanted.clear();
    for (let i = waiting.length - 1; i >= 0; i--) if (!waiting[i].name) waiting.splice(i, 1);     // the names still wanted stay in line
    for (const ref of all) forget(ref);
    waiting.unshift(...all.map(valueJob));        // values before the names
    pump();
  }
  // Numbers 0..99 are the presets (memory area 1), 100..199 the user's (area 2); chains are the user's only. The
  // user's names change with every WRITE, so they are not kept: asked again for what is chosen and when a list opens.
  const nameJob = (kind, n) => {
    const [ct, pid] = NAMES[kind], user = kind === 'chain' || n >= 100, set = kind === 'chain' ? n : n % 100;
    return { id: `${ct}:${user ? 2 : 1}:${set}:${pid}:0`, addr: addr(ct, pid, 0, user ? 2 : 1, set, 11), tries: 0, name: [kind, n] };
  };
  function wantNames() {
    const missing = Object.keys(NAMES).filter(kind => kind !== 'chain').flatMap(kind => Array.from({ length: 100 }, (_, n) => [kind, n]).filter(([, n]) => names[kind][n] == null));
    if (nameAt && linked) {
      // the presets' names from flash, twelve characters each: a request for each kind
      for (const kind of new Set(missing.map(([kind]) => kind))) {
        nameAt[kind].forEach((addr, n) => nameOf.set(addr, [kind, n]));
        sendMidi([0xF0, 0x7D, 0x58, 0x50, ...nameAt[kind].flatMap(addr => [...seven(addr, 5), 12, 0]), 0xF7]);
      }
      return;
    }
    for (const [kind, n] of missing) waiting.push(nameJob(kind, n));
    pump();
  }
  // the name of what is chosen now goes to the front of the line
  function wantName(kind, n) {
    if (n == null || names[kind][n] != null) return;
    waiting.unshift(nameJob(kind, n)); pump();
  }
  function wantUser(kind) {
    const from = kind === 'chain' ? 0 : 100;
    for (let n = from + 99; n >= from; n--) if (names[kind][n] == null) waiting.unshift(nameJob(kind, n));
    pump();
  }
  function forgetUser() {
    for (const kind of Object.keys(names)) names[kind].length = kind === 'chain' ? 0 : Math.min(names[kind].length, 100);
  }
  let reopen = null, reopenTimer = 0;       // the list that is open, drawn again when the user's names have come
  let bank = null, saveTimer = 0, rereadTimer = 0, shown = false;
  window.synthTaps.push(msg => {
    if (msg[1] === 0x7D && msg[2] === 0x58 && msg[3] === 0x50) {
      // memory from the emulator: this page's block of values, or one of the names
      const addr = msg.slice(4, 9).reduceRight((a, b) => a * 128 + b, 0);
      const bytes = Array.from({ length: (msg.length - 10) >> 1 }, (_, i) => msg[9 + 2 * i] << 4 | msg[10 + 2 * i]);
      if (span && addr === span[0] && cellOf) { peeked(bytes); return true; }
      if (!nameOf.has(addr)) return false;
      const [kind, n] = nameOf.get(addr);
      names[kind][n] = String.fromCharCode(...bytes.map(c => c >= 32 && c < 127 ? c : 32)).trim();
      clearTimeout(saveTimer);
      saveTimer = setTimeout(() => {
        const presets = Object.fromEntries(Object.entries(names).map(([k, list]) => [k, k === 'chain' ? [] : list.slice(0, 100)]));
        try { localStorage.setItem('xwp1:names:1', JSON.stringify(presets)); } catch (e) { /* kept for this visit only */ }
        redraw();
      }, 100);
      return true;
    }
    if (msg[0] === 0xB0 && msg[1] === 0) bank = msg[2];
    // the instrument says so when its Performance changes (bank 0x70 preset, 0x71 user): everything here is new
    // (only while this tab is shown: the answers share MIDI OUT with whatever the instrument sends meanwhile)
    if (msg[0] === 0xC0) {
      if (shown && (bank === 0x70 || bank === 0x71)) { clearTimeout(rereadTimer); rereadTimer = setTimeout(readAll, 500); }
      bank = null;        // a later program change without its own bank select is a tone's
    }
    if (msg.length < 26 || msg[0] !== 0xF0 || msg[1] !== 0x44 || msg[5] !== 1) return false;
    const a = msg.subarray(6, 24), pid = a[12] | a[13] << 7, id = `${a[0]}:${a[1]}:${a[2] | a[3] << 7}:${pid}:${a[10]}`;
    const job = flying.get(id);
    if (job) { clearTimeout(job.timer); flying.delete(id); }
    const data = msg.subarray(24, msg.length - 1);
    if (a[1] === 1 || a[1] === 2) {
      if (!job || !job.name) return false;
      const [kind, n] = job.name;
      const text = String.fromCharCode(...[...data].map(c => c >= 32 && c < 127 ? c : 32)).trim();
      if (!text) {
        // blank while the instrument is loading a Performance: asked again a little later
        if ((job.blank = (job.blank || 0) + 1) < 4) setTimeout(() => { waiting.unshift({ ...job, tries: 0 }); pump(); }, 700);
        pump();
        return true;
      }
      names[kind][n] = text;
      clearTimeout(saveTimer);
      saveTimer = setTimeout(() => {
        const presets = Object.fromEntries(Object.entries(names).map(([k, list]) => [k, k === 'chain' ? [] : list.slice(0, 100)]));
        try { localStorage.setItem('xwp1:names:1', JSON.stringify(presets)); } catch (e) { /* kept for this visit only */ }
        redraw();
      }, 300);
      if (a[1] === 2 && reopen) { clearTimeout(reopenTimer); reopenTimer = setTimeout(() => { if (popover.hidden) reopen = null; else reopen(); }, 500); }
      pump();
      return true;
    }
    const ref = byKey.get(`${a[0]}:${pid}:${a[10]}`);
    if (!ref || (!job && !shown)) return false;
    const value = decode(ref, data), want = wanted.get(ref.key);
    if (want != null && job) {
      // the read-back of an edit: the firmware now and then loses a write, which is then sent again
      wanted.delete(ref.key);
      if (value !== want.v && want.tries < 2) { sendMidi(setMessage(ref, want.v)); check(ref, want.v, want.tries + 1); }
      else if (vals.get(ref.key) === want.v) store(ref, value);
    } else if (want == null) {
      if (chosen && chosen.ref === ref && job) {
        // the choice is read back: now and then the instrument does not take one, and it is sent again
        if (value !== chosen.n && chosen.tries++ < 2) { select(ref, chosen.n); pump(); return true; }
        chosen = null;
      }
      store(ref, value);
    }
    pump();
    return true;          // the editor pages read the tone again when this view is left (`front.touch`)
  });
  // every edit made here is read back a moment after the last one
  const wanted = new Map();
  let holdTogglePending = false;
  watch(R.arpHold, v => {
    if (holdTogglePending && v != null) { holdTogglePending = false; edit(R.arpHold, 1 - v); }
  });
  function check(ref, v, tries = 0) {
    clearTimeout(wanted.get(ref.key)?.timer);
    wanted.set(ref.key, { v, tries, timer: setTimeout(() => { read(ref); pump(); }, 250) });
  }
  editHooks.push((ref, v) => { if (ref.key.startsWith('perf:')) { window.front.touch(); check(ref, v); } });

  // ---- controls
  const NOTE_NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
  const note = n => NOTE_NAMES[n % 12] + (Math.floor(n / 12) - 1);
  const label = (kind, n) => n == null ? '–' : names[kind][n] ?? (kind === 'chain' || n >= 100 ? `User ${kind === 'chain' ? n : n - 100}` : `${kind === 'perf' ? 'Performance' : kind} ${n}`);
  const bankNo = n => `${n < 100 ? 'P' : 'U'}:${Math.floor(n % 100 / 10)}-${n % 10}`;
  const redraws = [];
  const redraw = () => redraws.forEach(fn => fn());
  function paintPreset() {
    if (!document.body.classList.contains('perform-mode')) return;
    const n = vals.get(R.number.key);
    $('#presetNum').textContent = n == null ? '---' : bankNo(n);
    $('#presetName').textContent = n == null ? 'Reading Performance…' : label('perf', n);
  }
  watch(R.number, paintPreset);
  redraws.push(paintPreset);
  function openPreset() {
    wantUser('perf');
    const open = () => chooser($('#preset'), Array.from({ length: 200 }, (_, n) => label('perf', n)),
      vals.get(R.number.key), n => choose(R.number, n), { what: 'Performances', cols: true, width: 720 });
    reopen = open;
    open();
  }
  function stepPreset(by) {
    const n = vals.get(R.number.key);
    if (n != null) choose(R.number, (n + by + 200) % 200);
  }
  function byName(ref, kind, what, count = 200) {
    const num = el('small'), name = el('b');
    const b = el('button', { class: 'pick pe-pick', title: ref.name }, num, name, el('i', { text: '▾' }));
    const paint = () => { const v = vals.get(ref.key); num.textContent = v == null ? '' : kind === 'perf' ? bankNo(v) : String(v).padStart(count > 100 ? 3 : 2, '0'); name.textContent = label(kind, v); };
    watch(ref, v => { paint(); wantName(kind, v); }); redraws.push(paint);
    const open = () => chooser(b, Array.from({ length: count }, (_, n) => label(kind, n)), vals.get(ref.key), n => choose(ref, n), { what, cols: true, width: 620 });
    b.addEventListener('click', e => { e.stopPropagation(); wantUser(kind); reopen = open; open(); });
    const step = by => el('button', { class: 'pe-step', text: by < 0 ? '◀' : '▶', title: by < 0 ? 'Previous' : 'Next' });
    const [prev, next] = [step(-1), step(1)];
    prev.addEventListener('click', () => choose(ref, ((vals.get(ref.key) ?? 0) + count - 1) % count));
    next.addEventListener('click', () => choose(ref, ((vals.get(ref.key) ?? 0) + 1) % count));
    return el('div', { class: 'pe-choose' }, prev, b, next);
  }
  let chosen = null;        // a Performance or sequence asked for, until the instrument is seen to have it
  function select(ref, n) {
    // by bank select and program change (0x70 / 0x71 Performance, 0x72 / 0x73 step sequence; preset / user): set as a
    // parameter, the Performance loads but the instrument's display keeps the old number
    sendMidi([0xB0, 0, (ref === R.number ? 0x70 : 0x72) + (n >= 100 ? 1 : 0), 0xB0, 0x20, 0, 0xC0, n % 100]);
    clearTimeout(rereadTimer);
    // a Performance brings all its settings with it; a sequence its pattern
    rereadTimer = setTimeout(ref === R.number || cellOf ? readAll : () => { read(R.seq); read(R.pattern); pump(); }, cellOf ? 500 : 900);
  }
  function choose(ref, n) {
    if (vals.get(ref.key) == null) return;
    if (ref === R.number || ref === R.seq) {
      chosen = { ref, n, tries: 0 };
      store(ref, n);
      window.front.touch();
      select(ref, n);
    } else edit(ref, n);
  }
  // A number with - and +, for the two small signed values of a zone.
  function stepper(ref, title) {
    const out = el('output'), box = el('span', { class: 'pe-num', title });
    const btn = by => { const b = el('button', { text: by < 0 ? '−' : '+' }); b.addEventListener('click', () => edit(ref, (vals.get(ref.key) ?? 64) + by)); return b; };
    box.append(btn(-1), out, btn(1));
    watch(ref, v => { out.textContent = v == null ? '–' : (v > 64 ? '+' : '') + (v - 64); });
    box.addEventListener('wheel', e => { e.preventDefault(); edit(ref, (vals.get(ref.key) ?? 64) + (e.deltaY < 0 ? 1 : -1)); }, { passive: false });
    return box;
  }
  const flag = (ref, text, title) => { const b = el('button', { class: 'pe-flag', text, title });
    watch(ref, v => { b.classList.toggle('on', v === 1); b.classList.toggle('wait', v == null); });
    b.addEventListener('click', () => edit(ref, vals.get(ref.key) === 1 ? 0 : 1)); return b; };

  // The arpeggio on zone 1 as well: its switch, and the arpeggio's key range opened to the zone's keys. Zone 1 is
  // the part that several voices share, so with them every note of the arpeggio gets a voice of its own.
  function soloArp() {
    const b = el('button', { class: 'pe-flag', text: 'Zone 1', title: 'The arpeggio plays zone 1 too: with several voices, each of its notes on a voice of its own' });
    const z = Z[0], now = r => vals.get(r.key);
    const on = () => now(z.arp) === 1 && now(R.arpLo) <= now(z.lo) && now(R.arpHi) >= now(z.hi);
    const paint = () => { b.classList.toggle('on', on()); b.classList.toggle('wait', [z.arp, z.lo, z.hi, R.arpLo, R.arpHi].some(r => now(r) == null)); };
    for (const r of [z.arp, z.lo, z.hi, R.arpLo, R.arpHi]) watch(r, paint);
    b.addEventListener('click', () => {
      if (b.classList.contains('wait')) return;
      if (on()) { edit(z.arp, 0); return; }
      edit(z.arp, 1);
      if (now(R.arpLo) > now(z.lo)) edit(R.arpLo, now(z.lo));
      if (now(R.arpHi) < now(z.hi)) edit(R.arpHi, now(z.hi));
    });
    return b;
  }

  // An arpeggio per voice: with several voices the allocator gives every voice its note as a key of its own
  // keyboard (the player's "k2"), and each runs the arpeggio on it. Switching it on puts zone 1 into the arpeggio.
  function eachArp() {
    const b = el('button', { class: 'pe-flag', text: 'Each voice', title: 'With several voices: every voice runs its own arpeggio on the note it is given' });
    const paint = () => { b.classList.toggle('on', keysEach); b.hidden = poly.mode === 'multi' || poly.voices < 2; };
    keyWatch.push(paint); redraws.push(paint);
    b.addEventListener('click', () => {
      ws.send(keysEach ? 'k1' : 'k2');
      const zone1 = [...b.parentElement.querySelectorAll('.pe-flag')].find(x => x.textContent === 'Zone 1');
      if (!keysEach && zone1 && !zone1.classList.contains('on')) zone1.click();
    });
    paint();
    return b;
  }

  function toneName(n) {
    if (n == null) return '–';
    if (n < 100) return D.presets[n] ?? `Solo ${n}`;
    if (n < 150) return window.hexData?.presets[n - 100] ?? `Hex ${n - 100}`;
    if (n < 200) return window.drawData?.presets[n - 150] ?? `Organ ${n - 150}`;
    if (n >= 629 && n < 729) return `User Solo ${Math.floor((n - 629) / 10)}-${(n - 629) % 10}`;
    const p = window.pcmData;
    return (p && p.tones[n - p.first]) ?? `Tone ${n}`;
  }
  function toneEngine(n) {
    if (n == null) return '';
    // Preset tones are grouped by engine; user tones follow in the same order.
    if (n < 100 || (n >= 629 && n < 729)) return 'Solo Synth';
    if (n < 150) return 'Hex Layer';
    if (n < 200) return 'Drawbar Organ';
    if (n < 629) return 'PCM';
    return 'User tone';
  }

  // ---- the zones over the keys: a bar per zone, its ends dragged; arpeggio and phrase ranges below
  const LOW = 12, HIGH = 127;           // notes drawn (C0..G9); covers every on-screen keyboard position
  const COLORS = ['var(--gold)', 'var(--pitch)', 'var(--amp)', 'var(--lfo)'];
  function rangeBar(lo, hi, color, text, on) {
    const fill = el('i'), cap = el('span'), bar = el('div', { class: 'pe-bar', style: `--c:${color}` }, fill, cap);
    const paint = () => {
      const [a, b] = [vals.get(lo.key), vals.get(hi.key)];
      bar.classList.toggle('wait', a == null || b == null);
      bar.classList.toggle('off', !!on && vals.get(on.key) !== 1);
      if (a == null || b == null) return;
      const x = n => 100 * (clamp(n, LOW, HIGH + 1) - LOW) / (HIGH + 1 - LOW);
      const [from, to] = [x(Math.min(a, b)), x(Math.max(a, b) + 1)];
      fill.style.left = `${from}%`; fill.style.width = `${Math.max(0.6, to - from)}%`;
      // the note names go in the wider empty side, next to the bar
      cap.textContent = `${note(a)} \u2013 ${note(b)}`;
      const left = from > 100 - to, inside = to - from > 80;      // a bar over nearly all the keys carries them itself
      cap.classList.toggle('in', inside);
      cap.style.left = inside ? `calc(${from}% + 8px)` : left ? '' : `calc(${to}% + 6px)`;
      cap.style.right = inside || !left ? '' : `calc(${100 - from}% + 6px)`;
    };
    for (const r of [lo, hi, on].filter(Boolean)) watch(r, paint);
    const at = e => { const r = bar.getBoundingClientRect(); return clamp(Math.floor(LOW + (e.clientX - r.left) / r.width * (HIGH + 1 - LOW)), 0, 127); };
    let end = null;
    bar.addEventListener('pointerdown', e => {
      const [a, b] = [vals.get(lo.key), vals.get(hi.key)];
      if (a == null || b == null) return;
      e.preventDefault(); bar.setPointerCapture(e.pointerId);
      const n = at(e);
      end = Math.abs(n - a) <= Math.abs(n - b) ? lo : hi;       // the nearer end follows the pointer
      edit(end, end === lo ? Math.min(n, b) : Math.max(n, a));
    });
    bar.addEventListener('pointermove', e => {
      if (!end) return;
      const n = at(e), other = vals.get((end === lo ? hi : lo).key);
      edit(end, end === lo ? Math.min(n, other) : Math.max(n, other));
      const r = bar.getBoundingClientRect(); showTip(e.clientX, r.top, note(vals.get(end.key)));
    });
    for (const type of ['pointerup', 'pointercancel']) bar.addEventListener(type, () => { end = null; hideTip(); });
    return bar;
  }
  function keysPicture(label) {
    const keys = el('div', { class: 'pe-keys' }), names = el('div', { class: 'pe-octaves' });
    const drawn = [];
    for (let n = LOW; n <= HIGH; n++) {
      const black = [1, 3, 6, 8, 10].includes(n % 12);
      const key = el('i', { class: (black ? 'b' : 'w') + (n % 12 === 0 || n % 12 === 5 ? ' edge' : '') });
      keys.append(key); drawn.push(key);
      if (n % 12 === 0) names.append(el('span', { text: note(n), style: `left:${100 * (n - LOW) / (HIGH + 1 - LOW)}%` }));
    }
    const paint = () => {
      const shown = [...$('#keyboard').querySelectorAll('.key[data-note]')].map(key => Number(key.dataset.note));
      const first = Math.min(...shown), last = Math.max(...shown);
      label.textContent = shown.length ? `${shown.length} keys shown: ${note(first)}–${note(last)}` : 'On-screen keys';
      drawn.forEach((key, i) => key.classList.toggle('shown', LOW + i >= first && LOW + i <= last));
    };
    addEventListener('keyboard-range-change', paint);
    paint();
    return el('div', { class: 'pe-keybox' }, keys, names);
  }

  const CC_NAMES = { 1: 'Modulation', 2: 'Breath', 5: 'Portamento time', 7: 'Volume', 10: 'Pan', 11: 'Expression', 16: 'General 1', 17: 'General 2', 18: 'General 3',
    19: 'General 4', 64: 'Sustain', 65: 'Portamento', 66: 'Sostenuto', 67: 'Soft', 71: 'Resonance', 72: 'Release', 73: 'Attack', 74: 'Cutoff', 76: 'Vibrato rate',
    77: 'Vibrato depth', 78: 'Vibrato delay', 91: 'Reverb send', 93: 'Chorus send' };
  const CC = Array.from({ length: 128 }, (_, n) => CC_NAMES[n] ? `${n} ${CC_NAMES[n]}` : String(n));

  // ---- the mixer: the 16 parts as channel strips
  const ROLE = ['Zone 1', 'Zone 2', 'Zone 3', 'Zone 4', 'Phrase 1', 'Phrase 2', '', 'Drum 1', 'Drum 2', 'Drum 3', 'Drum 4', 'Drum 5', 'Bass', 'Solo 1', 'Solo 2', 'Chord'];
  function toneLabels() {
    const p = window.pcmData, out = [];
    for (let n = 0; n < (p ? p.first + p.tones.length : 200); n++) out.push(toneName(n));
    return out;
  }
  function level(ref) {
    const fill = el('b'), thumb = el('i'), out = el('output');
    const track = el('div', { class: 'fp-track', tabindex: 0, role: 'slider', 'aria-label': ref.name, 'aria-valuemin': 0, 'aria-valuemax': 127 }, fill, thumb);
    watch(ref, v => {
      const f = (v ?? 0) / 127;
      fill.style.height = `${f * 100}%`; thumb.style.bottom = `calc(${f} * (100% - 12px))`;
      out.textContent = v ?? '\u2013'; track.classList.toggle('wait', v == null); track.setAttribute('aria-valuenow', v ?? '');
    });
    const at = e => { const r = track.getBoundingClientRect(); edit(ref, 127 * (r.bottom - 6 - e.clientY) / (r.height - 12)); };
    let drag = false;
    track.addEventListener('pointerdown', e => { if (vals.get(ref.key) == null) return; e.preventDefault(); track.setPointerCapture(e.pointerId); drag = true; at(e); });
    track.addEventListener('pointermove', e => { if (drag) at(e); });
    for (const type of ['pointerup', 'pointercancel']) track.addEventListener(type, () => { drag = false; });
    track.addEventListener('wheel', e => { e.preventDefault(); edit(ref, (vals.get(ref.key) ?? 0) + (e.deltaY < 0 ? 1 : -1) * (e.shiftKey ? 8 : 1)); }, { passive: false });
    track.addEventListener('dblclick', () => edit(ref, 100));
    return el('div', { class: 'fp-fader mx-level' }, track, out);
  }
  function buildMixer() {
    const strips = M.map((part, p) => {
      const tone = el('button', { class: 'mx-tone' });
      watch(part.tone, v => { tone.textContent = toneName(v); tone.title = v == null ? '' : `Tone ${v}: choose another`; });
      tone.addEventListener('click', e => {
        e.stopPropagation();
        if (vals.get(part.tone.key) == null) return;
        chooser(tone, toneLabels(), vals.get(part.tone.key), n => edit(part.tone, n), { what: 'tones', cols: true, width: 760 });
      });
      const strip = el('div', { class: 'mx-strip' + (p < 4 ? ' zone' : p >= 7 ? ' seq' : ''), style: p < 4 ? `--c:${COLORS[p]}` : '' },
        el('div', { class: 'mx-head' }, el('b', { text: p + 1 }), led(part.on, `Part ${p + 1} on`)),
        el('span', { class: 'mx-role', text: ROLE[p] || '\u00a0' }), tone, level(part.vol),
        knob(part.pan, { label: 'Pan', size: 'sm' }), knob(part.rev, { label: 'Reverb', size: 'sm' }), knob(part.cho, { label: 'Chorus', size: 'sm' }));
      watch(part.on, v => strip.classList.toggle('off', v === 0));
      return strip;
    });
    return el('section', { class: 'group pe mix' },
      el('h2', {}, el('span', { text: 'Mixer' }), el('em', { text: 'the 16 parts: the zones\u2019 four, the phrases\u2019 and the step sequencer\u2019s' })),
      el('div', { class: 'mx-strips' }, strips));
  }

  function build(buttonOf) {
    const head = el('div', { class: 'pe-head' },
      el('div', { class: 'pe-block' }, el('h3', { text: 'Performance' }), byName(R.number, 'perf', 'Performances'),
        el('div', { class: 'pe-line' }, knob(R.tempo, { label: 'Tempo', size: 'sm' }), buttonOf('tap'),
          el('div', { class: 'pe-sends', title: 'The controller each knob sends to the zones that have it switched on' },
            K.map((ref, k) => pick(ref, CC, { label: `K${k + 1}`, what: 'controllers', cols: true, width: 620 }))))),
      el('div', { class: 'pe-block' }, el('h3', { text: 'Step sequence' }), byName(R.seq, 'seq', 'step sequences'),
        el('div', { class: 'pe-line' }, buttonOf('start'), buttonOf('chain'), field('Pattern', seg(R.pattern, ['1', '2', '3', '4', '5', '6', '7', '8']))),
        el('div', { class: 'pe-line' }, field('Pattern change', seg(R.seqTiming, ['Wait', 'At once'])), flag(R.seqShift, 'Key shift', 'The keyboard transposes the sequence')),
        el('div', { class: 'pe-line chain' }, el('span', { class: 'pe-cap', text: 'Chain' }), byName(R.chain, 'chain', 'chains', 100))),
      el('div', { class: 'pe-block' }, el('h3', { text: 'Arpeggio' }), byName(R.arp, 'arp', 'arpeggios'),
        el('div', { class: 'pe-line' }, buttonOf('arp'), flag(R.arpHold, 'Hold', 'Keep playing after the keys are released'), flag(R.arpShift, 'Key shift'), soloArp(), eachArp()),
        el('div', { class: 'pe-line' }, field('Sync to sequencer', seg(R.arpSync, ['Off', 'On', 'Start / stop'])))),
      el('div', { class: 'pe-block' }, el('h3', { text: 'Phrase' }), byName(R.phrase, 'phrase', 'phrases'),
        el('div', { class: 'pe-line' }, buttonOf('rec'), buttonOf('play')),
        el('div', { class: 'pe-line' }, flag(R.phraseKey, 'Key play', 'A key starts the phrase, transposed to it'))));
    const rows = Z.map((z, i) => {
      const tone = el('b', { class: 'pe-tone' });
      const engine = el('span', { class: 'pe-engine' });
      watch(z.tone, v => {
        const type = toneEngine(v);
        engine.textContent = type;
        tone.textContent = toneName(v);
        tone.title = v == null ? '' : `${type} tone ${v}, part ${i + 1} (MIDI channel ${i + 1})`;
      });
      return el('div', { class: 'pe-zone', style: `--c:${COLORS[i]}` },
        el('div', { class: 'pe-zhead' }, led(z.on, `Zone ${i + 1} on`),
          el('div', { class: 'pe-zinfo' }, el('div', { class: 'pe-zline' }, el('span', { class: 'pe-zname', text: `Zone ${i + 1}` }), engine), tone)),
        rangeBar(z.lo, z.hi, COLORS[i], `Zone ${i + 1}`, z.on),
        el('div', { class: 'pe-zctl' }, field('Oct', stepper(z.oct, 'Octave shift')), field('Tr', stepper(z.trans, 'Transpose, semitones')),
          flag(z.arp, 'Arp', 'The arpeggio plays this zone'), flag(z.phrase, 'Phr', 'Phrases play this zone'),
          el('span', { class: 'pe-flags' }, flag(z.k1, 'K1', 'Knob 1 acts on this zone'), flag(z.k2, 'K2', 'Knob 2'), flag(z.k3, 'K3', 'Knob 3'), flag(z.k4, 'K4', 'Knob 4'),
            flag(z.bend, 'Bend', 'Pitch bender'), flag(z.wheel, 'Mod', 'Modulation wheel'), flag(z.pedal, 'Ped', 'Pedal'))));
    });
    const extra = (title, lo, hi, color) => el('div', { class: 'pe-zone slim', style: `--c:${color}` },
      el('div', { class: 'pe-zhead' }, el('span', { class: 'pe-zname', text: title })), rangeBar(lo, hi, color, title), el('div', { class: 'pe-zctl' }));
    const keysLabel = el('span', { class: 'pe-zname dim' });
    const zones = el('div', { class: 'pe-zones' }, ...rows,
      extra('Arpeggio keys', R.arpLo, R.arpHi, 'var(--fx)'), extra('Phrase keys', R.phraseLo, R.phraseHi, 'var(--filter)'),
      el('div', { class: 'pe-zone slim keysrow' }, el('div', { class: 'pe-zhead' }, keysLabel), keysPicture(keysLabel), el('div', { class: 'pe-zctl' })));
    return el('section', { class: 'group pe' },
      el('h2', {}, el('span', { text: 'Performance' }), el('em', { text: 'what the keys play: four zones, a step sequence, an arpeggio and a phrase' })),
      head, zones);
  }

  watch(R.pattern, v => window.stepGrid.pattern(v));
  watch(R.seq, v => { if (v != null) window.stepGrid.changed(); });

  window.performanceEditor = {
    build, buildMixer,
    seqChooser: () => byName(R.seq, 'seq', 'step sequences'),
    patternSeg: () => seg(R.pattern, ['1', '2', '3', '4', '5', '6', '7', '8']),
    // `set`: 'edit' (the Performance) or 'mix' (the parts): what is read while it is shown
    show(on, set = 'edit') {
      shown = on; clearTimeout(rereadTimer); clearInterval(refresher);
      if (!on) { waiting.length = 0; return; }
      // from memory the values cost nothing: what was changed on the instrument's own panel shows within a second or so
      refresher = setInterval(() => { if (cellOf && linked && !wanted.size && !peeking) peek(); }, 1200);
      all = SETS[set];
      forgetUser();
      readAll();
      if (set !== 'mix') wantNames();
    },
    refs: R, zones: Z, parts: M, knobs: K, names: () => names, idle: () => !waiting.length && !flying.size && !wanted.size && !peeking && !chosen, choose, fast: () => !!cellOf, reread: readAll,
    toggleHold: () => {
      const v = vals.get(R.arpHold.key);
      if (v != null) edit(R.arpHold, 1 - v);
      else { holdTogglePending = !holdTogglePending; if (shown && linked) { read(R.arpHold); pump(); } }
    },
    paintPreset, openPreset, stepPreset,
  };
})();
