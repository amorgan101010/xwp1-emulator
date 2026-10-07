// Saving. The instrument keeps what WRITE stores in its user memory, and files on its SD card; here both are an
// image file the firmware reads and writes itself (xwp1/src/card.rs, the user memory in engine.rs). This page
//   - knows the user slots of every kind of tone (their tone numbers, their names read from the user memory),
//   - stores a tone, a Performance, a DSP, a step sequence, a chain, a phrase or an arpeggio in a slot through the
//     firmware's own WRITE dialog, pressing its buttons and reading its display,
//   - lists, downloads, uploads and deletes the card's files (F0 7D 58 43, `CARD` in engine.rs).
// Card Save / Card Load and All Data are done on the Front Panel view as on the instrument: tools/save_check.py
// goes through all of them.
(() => {
  const BUTTON = [0xF0, 0x7D, 0x58, 0x42], CARD = [0xF0, 0x7D, 0x58, 0x43], PEEK = [0xF0, 0x7D, 0x58, 0x50];
  const KEY = { write: 0x0C, enter: 0x1A, yes: 0x33, exit: 0x1C, tone: 0x15, perform: 0x0E, bank: 0x2F, menu: 0x27, down: 0x22,
    seq: 0x04, mixer: 0x29 };
  const POKE = [0xF0, 0x7D, 0x58, 0x57];
  const DIGIT = [0x36, 0x35, 0x34, 0x2B, 0x32, 0x26, 0x25, 0x2D, 0x2C, 0x24];
  const LED_TONE = 6, LED_PERFORM = 37, LED_SEQ = 5;
  // The WRITE screen keeps the address of the name it will store at 0x1c0052d8 and a copy for the display after
  // it (FINDINGS, "The WRITE screen's name"). The other kinds: `name` is that address (their edit buffer's name,
  // which is no parameter), `names(n)` the name of user slot n in the user memory, `hold` the button that opens
  // their screen when held and `led` the lamp that button leaves switched on.
  const WRITE_NAME = 0x1C0052D8;
  const OTHERS = {
    dsp: { title: 'DSP', name: 0x1C02D880, names: n => 0x18FDF004 + 38 * n },
    seq: { title: 'Step sequence', name: 0x1C005064, names: n => 0x18F21016 + 0x20000 * Math.floor(n / 31) + 4144 * (n % 31) },
    chain: { title: 'Sequence chain', name: 0x1C005228, names: n => 0x18FA0130 + 312 * n, hold: 0x00, led: 3 },
    phrase: { title: 'Phrase', name: 0x1C005138, names: n => 0x18F001AA + 40 * n, hold: 0x13 },
    arp: { title: 'Arpeggio', name: 0x1C00526C, names: n => 0x18FA79E4 + 200 * n, hold: 0x28, led: 34 },
  };
  // The user memory as the firmware lays it out (FINDINGS, "User memory"): a name per tone slot and per Performance
  const FIRST_USER = 629, TONE_NAMES = 0x18FC0009, TONE_STRIDE = 60, PERF_NAMES = 0x18FB00EC, PERF_STRIDE = 476;
  // the display's two fonts in the firmware image: eight rows a character, the dots from bit 7 down
  const FONTS = [0x180F1A0C - 0x47 * 8, 0x180F24FE - 0x65 * 8];
  // The user slots, by the tone button they belong to: tone numbers from `first`, `bank` the first bank digit the
  // WRITE dialog gives them, `presets` the factory tones stored there. VARIOUS holds the drum sets and other tones.
  const GROUPS = [
    { id: 'solo', engine: 'solo', title: 'Solo Synth', first: 629, count: 100, bank: 0, presets: [0, 100] },
    { id: 'hex', engine: 'hex', title: 'Hex Layer', first: 729, count: 50, bank: 0, presets: [100, 150] },
    { id: 'draw', engine: 'draw', title: 'Drawbar Organ', first: 779, count: 50, bank: 0, presets: [150, 200] },
    { id: 'piano', engine: 'pcm', title: 'Piano', first: 829, count: 20, bank: 0, presets: [200, 280] },
    { id: 'strings', engine: 'pcm', title: 'Strings / Brass', first: 849, count: 20, bank: 0, presets: [280, 380] },
    { id: 'guitar', engine: 'pcm', title: 'Guitar / Bass', first: 869, count: 20, bank: 0, presets: [380, 440] },
    { id: 'synth', engine: 'pcm', title: 'Synth', first: 889, count: 20, bank: 0, presets: [440, 540] },
    { id: 'drum', engine: 'pcm', title: 'Drum', first: 909, count: 10, bank: 0, presets: null },
    { id: 'various', engine: 'pcm', title: 'Various', first: 919, count: 20, bank: 1, presets: [540, 627] },
  ];
  const sleep = ms => new Promise(done => setTimeout(done, ms));
  const seven = (v, n) => Array.from({ length: n }, (_, i) => Math.floor(v / 128 ** i) % 128);
  const text = bytes => String.fromCharCode(...Array.from(bytes, c => c >= 32 && c < 127 ? c : 32)).trimEnd();

  // ---- what the engine and the firmware answer
  const peeks = new Map();      // address -> resolve
  const asked = new Map();      // "ct:pid" -> resolve
  let cardAnswer = null;
  window.synthTaps.unshift(msg => {
    if (msg[0] !== 0xF0) return false;
    if (msg[1] === 0x7D && msg[2] === 0x58 && msg[3] === 0x50) {
      const addr = msg.slice(4, 9).reduceRight((a, b) => a * 128 + b, 0), take = peeks.get(addr);
      if (!take) return false;
      peeks.delete(addr);
      take(Uint8Array.from({ length: (msg.length - 10) >> 1 }, (_, i) => msg[9 + 2 * i] << 4 | msg[10 + 2 * i]));
      return true;
    }
    if (msg[1] === 0x7D && msg[2] === 0x58 && msg[3] === 0x43) {
      if (cardAnswer) cardAnswer(msg);
      return true;
    }
    if (msg[1] === 0x44 && msg[5] === 1 && msg.length >= 26) {
      const key = `${msg[6]}:${msg[18] | msg[19] << 7}`, take = asked.get(key);
      if (!take) return false;
      asked.delete(key);
      take(msg.slice(24, msg.length - 1));
      return true;
    }
    return false;
  });
  // Memory of the first instance: [[address, length]...] -> Map(address -> bytes), null when no answer came. Asked
  // for in parts: the player drops what a page does not take in time, and each range is a message of its own.
  async function peek(ranges) {
    const got = new Map();
    for (let i = 0; i < ranges.length; i += 120) {
      const part = ranges.slice(i, i + 120);
      const all = await new Promise(done => {
        let left = part.length;
        const timer = setTimeout(() => { for (const [addr] of part) peeks.delete(addr); done(false); }, 2500);
        for (const [addr] of part) {
          peeks.set(addr, bytes => {
            got.set(addr, bytes);
            if (--left === 0) { clearTimeout(timer); done(true); }
          });
        }
        if (linked) sendMidi([...PEEK, ...part.flatMap(([addr, len]) => [...seven(addr, 5), ...seven(len, 2)]), 0xF7]);
      });
      if (!all) return null;
    }
    return got;
  }
  // A parameter of the edit buffer -> its value bytes, null when no answer came.
  function ask(ct, pid, length = 0) {
    if (!linked) return Promise.resolve(null);
    return new Promise(done => {
      const key = `${ct}:${pid}`;
      const timer = setTimeout(() => { asked.delete(key); done(null); }, 1500);
      asked.set(key, value => { clearTimeout(timer); done(value); });
      sendMidi([0xF0, 0x44, 0x16, 0x03, 0x7F, 0, ct, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, pid & 127, pid >> 7, 0, 0, length, 0, 0xF7]);
    });
  }
  const setName = (ct, pid, name, length) => sendMidi([0xF0, 0x44, 0x16, 0x03, 0x7F, 1, ct, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    pid & 127, pid >> 7, 0, 0, length - 1, 0, ...Array.from(name.padEnd(length).slice(0, length), c => c.charCodeAt(0) & 127), 0xF7]);

  // ---- the user slots
  const names = new Map();      // tone number -> name
  const perfNames = [];         // user Performance -> name
  const otherNames = Object.fromEntries(Object.keys(OTHERS).map(kind => [kind, []]));     // kind -> user slot -> name
  const groupOf = (n, drum = false) => n == null ? null
    : GROUPS.find(g => n >= g.first && n < g.first + g.count)
      ?? (n >= 540 && n < 627 && drum ? GROUPS.find(g => g.id === 'drum') : GROUPS.find(g => g.presets && n >= g.presets[0] && n < g.presets[1])) ?? null;
  const slotLabel = (g, i) => `U:${g.bank + Math.floor(i / 10)}-${i % 10}`;
  // A user tone -> { group, index, label: "U:0-3", name }, null for any other number.
  function slot(n) {
    const g = n == null ? null : GROUPS.find(g => n >= g.first && n < g.first + g.count);
    return g ? { group: g, index: n - g.first, label: slotLabel(g, n - g.first), name: names.get(n) ?? 'User tone' } : null;
  }
  // The user tones an editor's list offers after its presets: [[tone number, label]...].
  function listed(engine) {
    return GROUPS.filter(g => g.engine === engine).flatMap(g => Array.from({ length: g.count }, (_, i) =>
      [g.first + i, `${engine === 'pcm' ? g.title + ' ' : ''}${slotLabel(g, i)}  ${names.get(g.first + i) ?? ''}`.trimEnd()]));
  }
  let reading = null;
  // Read the slots' names from the user memory (again after every WRITE and Card Load).
  function refresh() {
    reading ??= (async () => {
      const tones = Array.from({ length: 310 }, (_, i) => [TONE_NAMES + TONE_STRIDE * i, 12]);
      const perfs = Array.from({ length: 100 }, (_, i) => [PERF_NAMES + PERF_STRIDE * i, 12]);
      const others = Object.entries(OTHERS).flatMap(([kind, o]) => Array.from({ length: 100 }, (_, i) => [o.names(i), 12, kind, i]));
      const got = await peek([...tones, ...perfs, ...others]);
      reading = null;
      if (!got) return false;
      tones.forEach(([addr], i) => names.set(FIRST_USER + i, text(got.get(addr))));
      perfs.forEach(([addr], i) => { perfNames[i] = text(got.get(addr)); });
      for (const [addr, , kind, i] of others) otherNames[kind][i] = text(got.get(addr));
      window.showProgram?.();
      return true;
    })();
    return reading;
  }

  // ---- the firmware's display, read with its own fonts
  let glyphs = null;
  async function fonts() {
    if (glyphs) return true;
    const got = await peek(FONTS.map(font => [font + 0x21 * 8, 94 * 8]));
    if (!got) return false;
    glyphs = [];
    for (const font of FONTS) {
      const bytes = got.get(font + 0x21 * 8);
      for (let c = 0; c < 94; c++) {
        const rows = bytes.subarray(c * 8, c * 8 + 8), cols = [];
        for (let x = 0; x < 8; x++) if (rows.some(r => r >> (7 - x) & 1)) cols.push(x);
        if (cols.length) glyphs.push({ ch: String.fromCharCode(0x21 + c), w: cols.at(-1) - cols[0] + 1, x0: cols[0], rows });
      }
    }
    glyphs.sort((a, b) => b.w - a.w);
    return true;
  }
  // The display's four lines; what is not a character of the two fonts (titles, symbols) comes out as "~".
  function lines() {
    const ram = window.front.ram, dot = (x, y) => x < 72 && (ram[y * 9 + (x >> 3)] >> (7 - (x & 7)) & 1) === 1;
    return [0, 8, 16, 24].map(top => {
      const ruled = y => { for (let x = 0, run = 0; x < 72; x++) { run = dot(x, y) ? run + 1 : 0; if (run >= 16) return true; } return false; };
      const skip = Array.from({ length: 8 }, (_, y) => ruled(top + y));
      const on = (x, y) => !skip[y] && dot(x, top + y);
      const blank = x => { for (let y = 0; y < 8; y++) if (on(x, y)) return false; return true; };
      let out = '', gap = 0;
      for (let x = 0; x < 72;) {
        if (blank(x)) { gap++; x++; continue; }
        if (out && gap >= 4) out += ' ';
        gap = 0;
        const g = glyphs.find(g => x + g.w <= 72 && blank(x + g.w)
          && g.rows.every((r, y) => { for (let i = 0; i < g.w; i++) if ((r >> (7 - g.x0 - i) & 1) !== (on(x + i, y) ? 1 : 0)) return false; return true; }));
        if (g) { out += g.ch; x += g.w; } else { out += '~'; while (x < 72 && !blank(x)) x++; }
      }
      return out;
    });
  }
  const shows = what => lines().some(line => line.replace(/ /g, '').includes(what));
  async function until(test, ms) {
    for (let waited = 0; waited <= ms; waited += 100) {
      if (test()) return true;
      await sleep(100);
    }
    return false;
  }
  const lit = n => (window.front.leds() >> BigInt(n) & 1n) === 1n;
  async function press(code, after = 350, held = 60) {
    sendMidi([...BUTTON, code, 1, 0xF7]);
    await sleep(held);
    sendMidi([...BUTTON, code, 0, 0xF7]);
    await sleep(after);
  }
  const poke = (addr, bytes) => sendMidi([...POKE, ...seven(addr, 5), ...Array.from(bytes).flatMap(b => [b >> 4, b & 15]), 0xF7]);
  const chars = (name, length) => Array.from(name.padEnd(length).slice(0, length), c => c.charCodeAt(0) & 127);

  // ---- WRITE
  let busy = false;
  // What is to be stored -> { kind, title, group, slots: [[index, label, name]...], name }. `kind`: one of OTHERS;
  // without it the tone the editor shows, or the Performance in the Perform view ('tone' / 'perf').
  async function subject(kind = null) {
    if (kind) {
      const o = OTHERS[kind], got = await peek([[o.name, 12]]);
      return { kind, title: o.title, name: got ? text(got.get(o.name)) : '',
        slots: Array.from({ length: 100 }, (_, i) => [i, `U:${Math.floor(i / 10)}-${i % 10}`, otherNames[kind][i] ?? '']) };
    }
    if (document.body.classList.contains('perform-mode') || (document.body.classList.contains('panel-mode') && lit(LED_PERFORM))) {
      const current = await ask(2, 0x96, 11);
      return { kind: 'perf', title: 'Performance', name: current ? text(current) : '',
        slots: Array.from({ length: 100 }, (_, i) => [i, `U:${Math.floor(i / 10)}-${i % 10}`, perfNames[i] ?? '']) };
    }
    // the part's tone as the firmware has it: a program change from elsewhere does not tell the page
    const number = await ask(2, 0x69), type = await ask(3, 6), current = await ask(3, 7, 11);
    const n = number ? number[0] | number[1] << 7 : vals.get(toneNumber().key);
    const group = groupOf(n, type && type[0] === 2);
    if (!group) return null;
    return { kind: 'tone', title: `${group.title} tone`, group, name: current ? text(current) : '',
      at: n >= group.first && n < group.first + group.count ? n - group.first : null,
      slots: Array.from({ length: group.count }, (_, i) => [i, slotLabel(group, i), names.get(group.first + i) ?? '']) };
  }
  // Store what `what` describes in its slot `index` under `name`. Throws an Error that says what went wrong.
  async function write(what, index, name) {
    if (busy) throw new Error('A save is still under way.');
    busy = true;
    try {
      if (!linked) throw new Error('The emulator is not connected.');
      if (!await fonts()) throw new Error('The display could not be read: this player does not answer memory requests.');
      // what the page has still to send or to read comes first: WRITE stores the edit buffer as it is then
      const settled = () => queue.length === 0 && inflight.size === 0 && later.size === 0 && written.size === 0 && checking.size === 0
        && (what.kind !== 'perf' || window.performanceEditor.idle());
      if (!await until(settled, 10000)) throw new Error('The page is still sending edits to the instrument; try again in a moment.');
      if (OTHERS[what.kind]) { await writeOther(what, index, name); return; }
      const [led, key] = what.kind === 'perf' ? [LED_PERFORM, KEY.perform] : [LED_TONE, KEY.tone];
      if (what.kind === 'tone') {
        // the macro LFO moves values of the edit buffer: WRITE would store them where they happen to be
        sendMidi([0xF0, 0x7D, 0x58, 0x4D, 0xF7]);
        await sleep(300);
        // the Tone button brings the display back to what the part plays; it leaves the edits alone
        await press(key);
      } else if (!lit(led)) {
        // the Performance's own part levels come with the mode (the Perform view switches to it when it opens)
        await press(key);
        await sleep(1200);
      }
      if (!await until(() => lit(led), 1500)) throw new Error(`The instrument did not go to ${what.kind === 'perf' ? 'Performance' : 'Tone'} mode.`);
      if (what.kind === 'perf') setName(2, 0x96, name, 12); else setName(3, 7, name, 12);
      await sleep(250);
      let open = false;
      for (let attempt = 0; attempt < 3 && !open; attempt++) {
        await press(KEY.write);
        open = await until(() => shows('PressEnter'), 1200);
      }
      if (!open) throw new Error(`The instrument did not open its Write screen (its display: ${lines().join(' / ')}).`);
      const bank = (what.group?.bank ?? 0) + Math.floor(index / 10);
      await press(KEY.bank);
      await press(DIGIT[bank]);
      await press(DIGIT[index % 10]);
      await press(KEY.enter);
      if (!await until(() => shows('Sure?') || shows('Replace?'), 1500)) {
        await press(KEY.exit);
        throw new Error(`The instrument did not ask to confirm (its display: ${lines().join(' / ')}).`);
      }
      await press(KEY.yes, 600);
      await until(() => !shows('Sure?') && !shows('Replace?') && !shows('Wait'), 6000);
      await sleep(900);         // the flash write, and the engine's report of it
      if (!await refresh()) throw new Error('The slot could not be read back.');
      const stored = what.kind === 'perf' ? perfNames[index] : names.get(what.group.first + index);
      if ((stored ?? '').trimEnd() !== name.trimEnd()) {
        throw new Error(`The slot holds “${stored}”, not “${name}”: nothing was stored (the display: ${lines().join(' / ')}).`);
      }
      // the instrument is on the user slot now, and the editor follows it
      if (what.kind === 'tone') store(toneNumber(), what.group.first + index);
    } finally {
      busy = false;
      if (what.kind === 'tone') window.macros?.syncLfo();
    }
  }

  // A DSP, step sequence, chain, phrase or arpeggio: to its screen, WRITE, the name into the edit buffer the
  // screen points at, the slot, ENTER, YES; then back to where the instrument was.
  async function writeOther(what, index, name) {
    const o = OTHERS[what.kind];
    const was = { mode: lit(LED_PERFORM) ? KEY.perform : lit(LED_SEQ) ? KEY.seq : KEY.tone, led: o.led != null && lit(o.led) };
    let depth = 0;        // screens to leave with EXIT afterwards
    try {
      if (what.kind === 'seq') {
        if (!lit(LED_SEQ)) await press(KEY.seq);
        if (!await until(() => lit(LED_SEQ), 1500)) throw new Error('The instrument did not go to the step sequencer.');
      } else if (what.kind === 'dsp') {
        const tone = slotOrPreset(vals.get(toneNumber().key)), line = await ask(2, 0x72);
        if (tone?.engine === 'solo') throw new Error('A Solo Synth tone keeps its effect in the tone itself: save the tone.');
        if (!line || line[0] !== 1) throw new Error('This tone goes to the chorus, not to a DSP: switch its effect line to DSP first.');
        await press(KEY.tone);
        if (!await until(() => lit(LED_TONE), 1500)) throw new Error('The instrument did not go to Tone mode.');
        await press(KEY.mixer, 500, 1500);        // held: the EFFECT screen
        depth = 1;
        for (let i = 0; i < 6; i++) await press(KEY.down, 200);
        if (!await until(() => shows('Cho/DSP'), 1500)) throw new Error(`The EFFECT screen did not come (the display: ${lines().join(' / ')}).`);
        await press(KEY.yes);                     // the screen's own switch: DSP, as the part already is
        await press(KEY.down, 250);
        await press(KEY.enter, 500);
        depth = 2;
      } else {
        await press(o.hold, 500, 1500);           // held: the kind's screen
        depth = 1;
      }
      let open = false;
      for (let attempt = 0; attempt < 3 && !open; attempt++) {
        await press(KEY.write);
        open = await until(() => shows('PressEnter'), 1200);
      }
      if (!open) throw new Error(`The instrument did not open its Write screen (its display: ${lines().join(' / ')}).`);
      depth++;
      // which name it is about to store says which screen WRITE was pressed on
      const got = await peek([[WRITE_NAME, 4]]), at = got ? got.get(WRITE_NAME).reduceRight((a, b) => a * 256 + b, 0) : null;
      if (at !== o.name) throw new Error(`The instrument is not on the ${o.title} screen (its display: ${lines().join(' / ')}).`);
      poke(o.name, chars(name, what.kind === 'dsp' ? 16 : 12));
      poke(WRITE_NAME + 4, chars(name, 12));
      await sleep(150);
      await press(KEY.bank);
      await press(DIGIT[Math.floor(index / 10)]);
      await press(DIGIT[index % 10]);
      await press(KEY.enter);
      if (!await until(() => shows('Sure?') || shows('Replace?'), 1500)) throw new Error(`The instrument did not ask to confirm (its display: ${lines().join(' / ')}).`);
      await press(KEY.yes, 600);
      depth--;
      await until(() => !shows('Sure?') && !shows('Replace?') && !shows('Wait'), 6000);
      await sleep(900);
      if (!await refresh()) throw new Error('The slot could not be read back.');
      const stored = otherNames[what.kind][index];
      if ((stored ?? '').trimEnd() !== name.trimEnd()) {
        throw new Error(`The slot holds “${stored}”, not “${name}”: nothing was stored (the display: ${lines().join(' / ')}).`);
      }
    } finally {
      // back to where the instrument was: out of the screens, the mode, and the switch a held button left on
      for (; depth > 0; depth--) await press(KEY.exit, 300);
      if (what.kind === 'seq' && was.mode !== KEY.seq) {
        // the mode it was in; a button pressed while "Complete!" still shows is not always taken
        const led = was.mode === KEY.perform ? LED_PERFORM : LED_TONE;
        for (let attempt = 0; attempt < 3 && !lit(led); attempt++) { await press(was.mode); await until(() => lit(led), 800); }
      }
      if (o.led != null) { await sleep(200); if (lit(o.led) !== was.led) await press(o.hold); }
    }
  }
  const slotOrPreset = n => groupOf(n);

  // ---- the card's files
  function card(op, name = '', bytes = null) {
    if (!linked) return Promise.resolve(null);
    return new Promise(done => {
      const timer = setTimeout(() => { cardAnswer = null; done(null); }, 4000);
      cardAnswer = msg => {
        clearTimeout(timer); cardAnswer = null;
        const body = msg.slice(5, msg.length - 1);
        if (msg[4] === 1) {
          const end = body.indexOf(0);
          done({ file: Uint8Array.from({ length: (body.length - end - 1) >> 1 }, (_, i) => body[end + 1 + 2 * i] << 4 | body[end + 2 + 2 * i]) });
        } else if (msg[4] === 0x7F) done({ error: `${text(body)} could not be read.` });
        else { try { done(JSON.parse(String.fromCharCode(...body))); } catch (e) { done(null); } }
      };
      const out = [...CARD, op];
      if (op) {
        out.push(...Array.from(name, c => c.charCodeAt(0) & 127), 0);
        if (bytes) for (const b of bytes) out.push(b >> 4, b & 15);
      }
      sendMidi([...out, 0xF7]);
    });
  }
  // What each kind of file holds, by the three letters the instrument gives it.
  const FILE_KINDS = { ZPF: 'Performance', ZSY: 'Solo Synth tone', ZLT: 'Hex Layer tone', ZDO: 'Drawbar Organ tone', ZTN: 'PCM melody tone',
    ZDR: 'PCM drum tone', DS7: 'DSP', ZSS: 'Step sequence', ZSC: 'Sequence chain', ZAR: 'Arpeggio', ZPH: 'Phrase', ZAL: 'All data',
    ZST: 'Settings', MID: 'Music file', WAV: 'Audio file' };
  const fileName = name => {
    const up = name.trim().toUpperCase(), dot = up.lastIndexOf('.');
    const fits = (part, most) => part.length >= 1 && part.length <= most && /^[A-Z0-9$&_'()\-^{}@~`]+$/.test(part);
    return dot > 0 && fits(up.slice(0, dot), 8) && fits(up.slice(dot + 1), 3) ? up : null;
  };

  // ---- the dialog
  function dialog(anchor) {
    const status = el('div', { class: 'st-status', role: 'status' });
    const say = (msg, bad = false) => { status.textContent = msg; status.classList.toggle('bad', bad); };
    const head = el('div', { class: 'head', text: 'Save to user memory' });
    const nameBox = el('input', { class: 'st-name', maxlength: 12, spellcheck: 'false', 'aria-label': 'Name', placeholder: 'Name' });
    const list = el('div', { class: 'st-slots', role: 'listbox', 'aria-label': 'User slot' });
    const save = el('button', { class: 'pill st-go', text: 'Save' });
    const files = el('div', { class: 'st-files' });
    const upload = el('input', { type: 'file', multiple: '', hidden: '' });
    const pick = el('button', { class: 'pill', text: 'Upload files…', title: 'Put files from this computer on the card (names of eight characters, a dot, three)' });
    const native = el('button', { class: 'pill', text: 'Card Save / Load…', title: 'Open the instrument’s own menu on the Front Panel: Card Save, Card Load, Clear User' });
    let what = null, chosen = 0, kind = null;
    const kinds = el('div', { class: 'st-kinds' }, ...[[null, 'This sound'], ['dsp', 'DSP'], ['seq', 'Sequence'], ['chain', 'Chain'], ['phrase', 'Phrase'], ['arp', 'Arpeggio']]
      .map(([id, label]) => {
        const b = el('button', { class: 'pill' + (id === kind ? ' on' : ''), text: label,
          title: id ? `Store the ${OTHERS[id].title.toLowerCase()} the instrument has loaded` : 'Store the tone being edited, or the Performance in the Perform view' });
        b.addEventListener('click', () => {
          kind = id;
          for (const other of kinds.children) other.classList.toggle('on', other === b);
          nameBox.value = ''; say('');
          describe();
        });
        return b;
      }));

    function slots() {
      list.replaceChildren(...what.slots.map(([i, label, name]) => {
        const item = el('button', { class: 'item' + (i === chosen ? ' sel' : ''), role: 'option', 'aria-selected': i === chosen }, el('small', { text: label }), name || '—');
        item.addEventListener('click', () => { chosen = i; slots(); });
        return item;
      }));
      save.textContent = `Save to ${what.slots[chosen][1]}`;
      const sel = list.querySelector('.sel');
      if (sel) sel.scrollIntoView({ block: 'nearest' });
    }
    async function describe() {
      await refresh();
      what = await subject(kind);
      if (!what) { head.textContent = 'Save to user memory'; say('Nothing to save here: no tone is loaded yet.', true); save.disabled = true; return; }
      head.textContent = `Save this ${what.title}`;
      if (!nameBox.value) nameBox.value = what.name;
      chosen = what.at ?? Math.max(0, what.slots.findIndex(([, , name]) => !name || name === 'Untitled'));
      (kinds.children[0]).textContent = what.kind === 'perf' ? 'Performance' : what.kind === 'tone' ? 'Tone' : kinds.children[0].textContent;
      save.disabled = false;
      slots();
    }
    save.addEventListener('click', async () => {
      const name = nameBox.value.replace(/[^\x20-\x7e]/g, '').trimEnd();
      if (!name) { say('Give it a name first.', true); nameBox.focus(); return; }
      save.disabled = true;
      say(`Storing “${name}” in ${what.slots[chosen][1]}…`);
      try {
        await write(what, chosen, name);
        say(`Stored “${name}” in ${what.slots[chosen][1]}.`);
        what.slots[chosen][2] = name;
        slots();
      } catch (e) {
        say(e.message, true);
      }
      save.disabled = false;
    });

    function showFiles(answer) {
      if (!answer) { files.replaceChildren(el('div', { class: 'st-none', text: 'This player does not answer for the card.' })); return; }
      if (answer.error) say(answer.error, true);
      if (!answer.card) { files.replaceChildren(el('div', { class: 'st-none', text: 'No card in the slot (the player was started without one).' })); return; }
      if (!answer.files.length) { files.replaceChildren(el('div', { class: 'st-none', text: 'The card is empty. The instrument’s Card Save puts files here; so does Upload.' })); return; }
      files.replaceChildren(...answer.files.map(([name, size]) => {
        const get = el('button', { class: 'st-act', text: 'Download', title: `Save ${name} to this computer` });
        const del = el('button', { class: 'st-act', text: 'Delete', title: `Delete ${name} from the card` });
        get.addEventListener('click', async () => {
          const got = await card(1, name);
          if (!got || !got.file) { say(got?.error ?? `${name} could not be read.`, true); return; }
          const a = el('a', { href: URL.createObjectURL(new Blob([got.file])), download: name });
          a.click();
          setTimeout(() => URL.revokeObjectURL(a.href), 5000);
        });
        let sure = 0;
        del.addEventListener('click', async () => {
          if (!sure) { del.textContent = 'Really delete?'; sure = setTimeout(() => { sure = 0; del.textContent = 'Delete'; }, 3000); return; }
          clearTimeout(sure);
          showFiles(await card(3, name));
        });
        return el('div', { class: 'st-file' }, el('b', { text: name }), el('span', { text: FILE_KINDS[name.split('.').pop()] ?? '' }),
          el('small', { text: size < 1024 ? `${size} B` : `${(size / 1024).toFixed(1)} kB` }), get, del);
      }));
    }
    pick.addEventListener('click', () => upload.click());
    upload.addEventListener('change', async () => {
      let last = null;
      for (const file of upload.files) {
        const name = fileName(file.name);
        if (!name) { say(`${file.name}: the instrument reads names of eight characters, a dot and three (letters, digits, - _ and a few more).`, true); continue; }
        if (file.size > 4 << 20) { say(`${file.name} is too large to send this way; copy it into the card image directly.`, true); continue; }
        last = await card(2, name, new Uint8Array(await file.arrayBuffer()));
      }
      upload.value = '';
      if (last) showFiles(last);
    });
    native.addEventListener('click', async () => {
      closePopover();
      window.front.showPanel(true);
      await press(KEY.menu);
    });

    popover.replaceChildren(el('div', { class: 'store' },
      el('div', { class: 'st-col' }, head, kinds,
        el('div', { class: 'st-row' }, nameBox, save), list, status,
        el('div', { class: 'st-note', text: 'This presses the instrument’s own WRITE for you, on the screen of what you chose, and puts the instrument back where it was.' })),
      el('div', { class: 'st-col' }, el('div', { class: 'head', text: 'SD card' }), files,
        el('div', { class: 'st-row' }, pick, native, upload),
        el('div', { class: 'st-note', text: 'Files are the instrument’s own (.ZSY, .ZLT, .ZPF …): the same a real XW-P1 writes to its card.' }))));
    popover.hidden = false;
    popover.style.width = '';
    const r = anchor.getBoundingClientRect(), w = popover.offsetWidth;
    popover.style.left = clamp(r.right - w, 12, innerWidth - w - 12) + 'px';
    popover.style.top = r.bottom + 6 + 'px';
    describe();
    card(0).then(showFiles);
  }

  const button = $('#writeUser');
  button.addEventListener('click', e => {
    e.stopPropagation();
    if (!popover.hidden && popover.querySelector('.store')) { closePopover(); return; }
    dialog(button);
  });

  window.userSlots = { GROUPS, OTHERS, peek, slot, listed, refresh, names: () => names, perfNames: () => perfNames, otherNames: () => otherNames, write, subject, lines, fonts, card, groupOf };
  window.storeLinked = () => { names.clear(); refresh(); };
})();
