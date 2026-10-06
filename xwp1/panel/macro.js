// XW-P1 panel: Solo Synth macros. One control here moves many of the tone's
// parameters. The instrument knows nothing of this: the tone holds the
// resulting values only. What the page itself has to remember (linked
// envelopes, the envelope view, controller numbers) is kept by the browser.
'use strict';
(() => {
const LANES = { pitch: 'tssOSCPENV', filter: 'tssOSCFENV', amp: 'tssOSCAENV' };
const CLOCKS = { pitch: 'tssOSCPEclk', filter: 'tssOSCFEclk', amp: 'tssOSCAEclk' };
const FIELDS = ['iL', 'aT', 'aL', 'dT', 'sL', 'r1T', 'r1L', 'r2T', 'r2L'];
const BLOCKS = [0, 1, 2, 3, 4, 5], PITCHED = [0, 1, 2, 3];
const each = (id, insts = BLOCKS) => insts.map(i => solo(id, i));
const now = ref => vals.get(ref.key);

// ---------------------------------------------------------------- kept by the browser

let saved = {};
try { saved = JSON.parse(localStorage.getItem('xwp1.macros')) || {}; } catch (e) { /* no storage: nothing is kept */ }
const keep = () => { try { localStorage.setItem('xwp1.macros', JSON.stringify(saved)); } catch (e) { /* as above */ } };
let morphConfigs = {};
try {
  const stored = JSON.parse(localStorage.getItem('xwp1.morph-configs'));
  if (stored && typeof stored === 'object' && !Array.isArray(stored)) morphConfigs = stored;
} catch (e) { /* no saved morph configurations */ }
const keepMorphConfigs = () => { try { localStorage.setItem('xwp1.morph-configs', JSON.stringify(morphConfigs)); return true; } catch (e) { return false; } };
const byId = new Map();         // every macro and setting: window.macros.ref(id).set(value), for controllers and tests
let wavesReady = false;
let waveCommand = 0;
// A setting of the page, held like a parameter so that the ordinary switches can show it.
function setting(id, def, max = 1) {
  const ref = { key: 'macro:' + id, id, min: 0, max, vt: 'nf', name: id, setting: true, defaultValue: def };
  byId.set(id, ref);
  vals.set(ref.key, saved[id] ?? def);
  ref.set = v => { v = clamp(Math.round(v), 0, max); saved[id] = v; keep(); store(ref, v); };
  return ref;
}
const adsr = setting('adsr', 1);
const link = { pitch: setting('link.pitch', 1), filter: setting('link.filter', 1), amp: setting('link.amp', 1) };
const lane = setting('lane', 0, 3), lfo = setting('lfo', 0), open = setting('open', 1);
const amount = setting('amount', 15, 100);
const macroLfoTarget = setting('macroLfo.target', 0, 14);
const macroLfoMode = setting('macroLfo.mode', 0, 4);
const macroLfoShape = setting('macroLfo.shape', 0, 6);
const macroLfoRate = setting('macroLfo.rate', 45, 100);
const macroLfoDepth = setting('macroLfo.depth', 100, 100);
const macroLfoCenter = setting('macroLfo.center', 50, 100);
macroLfoTarget.name = 'Macro LFO destination. Morph requires both A and B snapshots.';
macroLfoMode.name = 'Hold samples the free-running wave separately for every note. Free continues running; Trig, One and Half restart for each note.';
macroLfoShape.name = 'Macro LFO waveform';
macroLfoRate.name = 'Macro LFO speed in hertz';
macroLfoDepth.name = 'How far the Macro LFO moves between its end states';
macroLfoCenter.name = 'The middle position of the Macro LFO destination';
const LFO_TARGETS = ['Off', 'Morph', 'Bright', 'Env amt', 'Mix', 'Spread', 'Glide', 'Velocity', 'Key follow',
  'Vibrato', 'Wobble', 'Tremolo', 'Attack', 'Decay', 'Release'];
let syncMacroLfo = () => {};
const LOCKS = [['Pitch', 'Pitch', 1], ['Filter', 'Block filters', 0], ['Amp', 'Amp', 0], ['TotalFilter', 'Total Filter', 0], ['LFO', 'LFOs', 0]];
const locks = Object.fromEntries(LOCKS.map(([part, , def]) => [part, setting('lock.' + part, def)]));
const frees = new Map();
// a block whose envelope goes its own way although its kind is linked
function free(kind, inst) {
  const id = `free.${kind}.${inst}`;
  if (!frees.has(id)) frees.set(id, setting(id, 0));
  return frees.get(id);
}

// ---------------------------------------------------------------- linked envelopes

// An envelope value changed by hand goes to the same envelope of every other block.
const kindOf = new Map();
for (const kind in LANES) {
  for (const f of FIELDS) kindOf.set(LANES[kind] + f, kind);
  kindOf.set(CLOCKS[kind], kind);
}
let recordLinkedEndpointEdit = () => {};
editHooks.push((ref, v) => {
  const kind = ref.ct === 9 && kindOf.get(ref.id);
  if (!kind || now(link[kind]) !== 1 || now(free(kind, ref.inst)) === 1) return;
  for (const j of BLOCKS) if (j !== ref.inst && now(free(kind, j)) !== 1) {
    const target = solo(ref.id, j), before = now(target);
    editLater(target, v);
    recordLinkedEndpointEdit(target, v, before);
  }
});

// ---------------------------------------------------------------- undo

let undo = null;
const undoButton = el('button', { class: 'pill', text: 'Undo', title: 'Take back the last macro edit, block copy, shape, ADSR tidy-up, mutation or morph (again: redo)', disabled: '' });
function snapshot() {
  const s = new Map();
  for (const r of refs.values()) if (r.ct === 9 && now(r) != null) s.set(r.key, now(r));
  return s;
}
const restore = s => { for (const [key, v] of s) editLater(refs.get(key), v); };
let captureUndoExtra = () => null, restoreUndoExtra = () => {};
const undoState = () => ({ tone: snapshot(), extra: captureUndoExtra() });
function remember() { undo = undoState(); undoButton.disabled = false; }
undoButton.addEventListener('click', () => {
  const s = undoState();
  restore(undo.tone);
  restoreUndoExtra(undo.extra);
  undo = s;
});

// ---------------------------------------------------------------- kinds of macro

const virt = (id, name, min, max) => { const ref = { key: 'macro:' + id, id, min, max, vt: 'nf', name }; byId.set(id, ref); return ref; };

// A knob that shows what the tone holds (worked out from `targets`) and writes all of them.
function derived(id, name, min, max, targets, get, set) {
  const ref = virt(id, name, min, max);
  const update = () => store(ref, targets.some(t => now(t) == null) ? null : get());
  for (const t of targets) watch(t, update);
  ref.set = v => { if (now(ref) != null) set(clamp(Math.round(v), min, max)); };
  return ref;
}

// A knob that shifts its targets from where they were: the values it started
// from are kept, so a value pushed against its limit comes back as it was.
// A target changed elsewhere meanwhile takes that as its new starting value.
// Loading a tone puts the knob back in the middle.
function trim(id, name, span, all, apply, invert) {
  const ref = virt(id, name, -span, span);
  const base = new Map(), sent = new Map();
  let m = 0, targets = all;
  const clear = () => { m = 0; base.clear(); sent.clear(); };
  const show = () => store(ref, targets.some(t => now(t) == null) ? null : m);
  for (const t of all) watch(t, v => {
    if (v == null) clear();
    else if (sent.has(t.key) && sent.get(t.key) !== v) { base.set(t.key, invert(v, m, t, base.get(t.key))); sent.set(t.key, v); }
    show();
  });
  ref.set = v => {
    if (now(ref) == null) return;
    m = clamp(Math.round(v), -span, span);
    for (const t of targets) {
      if (!base.has(t.key)) base.set(t.key, now(t));
      const to = clamp(Math.round(apply(base.get(t.key), m, t)), t.min, t.max);
      sent.set(t.key, to);
      macroWrite(t, to);
    }
    if (m === 0) clear();
    show();
  };
  ref.reset = () => { clear(); show(); };
  // other targets from now on: what the knob did so far stays, and it starts from the middle again
  ref.aim = list => { targets = list; clear(); show(); };
  return ref;
}
const add = [(b, m) => b + m, (v, m) => v - m];
let macroWrite = (ref, value) => editLater(ref, value);
let setVelocityValuesAction = () => false;

// ---------------------------------------------------------------- controllers

const knobs = [];               // { ref, box, tag }
let learning = false, chosen = null;
const learnButton = el('button', { class: 'pill', text: 'Learn', title: 'Give a macro knob to a MIDI controller: switch this on, touch the knob, move the controller' });
function tags() {
  for (const k of knobs) {
    const cc = (saved.cc || {})[k.ref.id];
    k.tag.hidden = cc == null && k !== chosen;
    k.tag.textContent = k === chosen ? 'move it' : `cc ${cc}` + (learning ? ' ×' : '');
    k.box.classList.toggle('chosen', k === chosen);
  }
}
function register(ref, box) {
  const k = { ref, box, tag: el('button', { class: 'cc', hidden: '' }) };
  box.append(k.tag);
  box.addEventListener('pointerdown', () => { if (learning) { chosen = k; tags(); } }, true);
  k.tag.addEventListener('click', () => { if (learning && k !== chosen) { delete saved.cc[ref.id]; keep(); tags(); } });
  knobs.push(k);
  return box;
}
learnButton.addEventListener('click', () => {
  learning = !learning;
  chosen = null;
  learnButton.classList.toggle('on', learning);
  document.body.classList.toggle('learning', learning);
  tags();
});
// A controller moved (on a keyboard the emulator listens to, or on its MIDI port).
function controller(cc, v) {
  if (learning) {
    if (!chosen || cc === 0 || cc === 32 || cc >= 120) return;
    saved.cc = { ...saved.cc, [chosen.ref.id]: cc };
    keep();
    chosen = null;
    tags();
    return;
  }
  for (const k of knobs) if ((saved.cc || {})[k.ref.id] === cc) k.ref.set(k.ref.min + (k.ref.max - k.ref.min) * v / 127);
}

// ---------------------------------------------------------------- envelopes: shapes, link

// init level, attack time / level, decay time, sustain level, release 1 time / level, release 2 time / level
const SHAPES = [['Gate', [0, 0, 127, 0, 127, 4, 0, 0, 0]], ['Pluck', [0, 0, 127, 48, 0, 36, 0, 0, 0]], ['Keys', [0, 0, 127, 84, 48, 44, 0, 0, 0]],
                ['Pad', [0, 72, 127, 64, 100, 84, 0, 0, 0]], ['Swell', [0, 104, 127, 0, 127, 60, 0, 0, 0]]];
const BENDS = [['Flat', [0, 0, 0, 0, 0, 0, 0, 0, 0]], ['Scoop up', [-24, 28, 0, 0, 0, 0, 0, 0, 0]], ['Fall in', [24, 28, 0, 0, 0, 0, 0, 0, 0]],
               ['Drop on release', [0, 0, 0, 0, 0, 48, -32, 0, -32]]];
// The two small buttons on an envelope. R: its nine parameters; kind: pitch / filter / amp for a block's, none for the Total Filter's.
function envelopeTools(R, kind, inst, bipolar) {
  const shapes = bipolar ? BENDS : SHAPES;
  const names = shapes.map(s => s[0]).concat(kind ? ['Copy to all blocks'] : []);
  const shape = el('button', { class: 'chip', text: 'Shape', title: 'Set this envelope to a ready-made shape' });
  shape.addEventListener('click', e => {
    e.stopPropagation();
    chooser(shape, names, -1, i => {
      remember();
      if (i < shapes.length) FIELDS.forEach((f, n) => edit(R[f], shapes[i][1][n]));
      else for (const j of BLOCKS) if (j !== inst) for (const f of FIELDS) macroWrite(solo(LANES[kind] + f, j), now(R[f]));
    }, { number: false });
  });
  if (!kind) return el('div', { class: 'gtools' }, shape);
  const chain = el('button', { class: 'chip' });
  const own = free(kind, inst);
  const show = () => {
    const on = now(link[kind]) === 1 && now(own) !== 1;
    chain.classList.toggle('on', on);
    chain.textContent = on ? 'linked' : 'own';
    chain.title = on ? `Changes here go to the ${kind} envelope of every linked block. Click to give this block its own.`
      : `This block’s ${kind} envelope is edited alone. Click to link it with the others.`;
  };
  watch(link[kind], show);
  watch(own, show);
  chain.addEventListener('click', () => {
    if (now(link[kind]) !== 1) { link[kind].set(1); own.set(0); } else own.set(now(own) === 1 ? 0 : 1);
  });
  return el('div', { class: 'gtools' }, chain, shape);
}

// ---------------------------------------------------------------- blocks: copy, swap, init, unison

const family = i => [0, 0, 1, 1, 2, 3][i];      // which blocks can take each other's wave
function blockValues(i) {
  const v = new Map();
  for (const p of D.params) if (p.count === 6 || (p.count === 2 && p.block === 'PWM' && i < 2)) v.set(p.id, now(solo(p.id, i)));
  return v;
}
function putBlock(to, values, from) {
  for (const [id, v] of values) {
    if (id === 'tssOSCsw' || v == null || (id === 'tssOSCwf' && family(to) !== from) || (P[id].block === 'PWM' && to > 1)) continue;
    macroWrite(solo(id, to), v);
  }
}
// a block at rest, as the instrument's own blank tone has it
const initValue = id => /keyfB$/.test(id) ? 60 : /^tssOSC[FA]ENV[as]L$/.test(id) ? 127
  : ({ tssOSCPkeyf: 64, tssOSCFcoff: 15, tssOSCAlvl: 100, tssOSCAtch: 32, tssOSCPlfo1D: 63, tssOSCPortaTm: 10 })[id] ?? 0;
function blockButton(i) {
  const others = BLOCKS.filter(j => j !== i), twin = i ^ 1;
  const names = [...others.map(j => `Copy to ${D.blocks[j]}`), ...others.map(j => `Swap with ${D.blocks[j]}`), 'Init this block',
                 ...(i < 4 ? [`Unison with ${D.blocks[twin]}`] : [])];
  const b = el('button', { class: 'chip', text: 'Block', title: 'Copy, swap or initialise this block' });
  b.addEventListener('click', e => {
    e.stopPropagation();
    chooser(b, names, -1, n => {
      remember();
      const mine = blockValues(i);
      if (n < 5) putBlock(others[n], mine, family(i));
      else if (n < 10) { const j = others[n - 5], theirs = blockValues(j); putBlock(j, mine, family(i)); putBlock(i, theirs, family(j)); }
      else if (n === 10) putBlock(i, new Map([...mine.keys()].filter(id => id !== 'tssOSCwf').map(id => [id, initValue(id)])), family(i));
      else {
        // the same sound twice, a little apart
        putBlock(twin, mine, family(i));
        const d = Math.max(8, Math.abs(now(solo('tssOSCPdtne', i))));
        macroWrite(solo('tssOSCPdtne', i), -d);
        macroWrite(solo('tssOSCPdtne', twin), d);
        for (const j of [i, twin]) macroWrite(solo('tssOSCsw', j), 1);
      }
    }, { number: false });
  });
  return b;
}

// ---------------------------------------------------------------- the strip

function build() {
  const sw = each('tssOSCsw');
  const active = () => PITCHED.filter(i => now(sw[i]) === 1);
  const tf = id => solo('tssFLT' + id);

  // --- tune
  const detune = each('tssOSCPdtne', PITCHED), offset = each('tssOSCPoset', PITCHED);
  const spread = derived('spread', 'Spread: detune the pitched blocks that are on, evenly to both sides', 0, 255, [...sw.slice(0, 4), ...detune],
    () => Math.max(0, ...active().map(i => Math.abs(now(detune[i])))),
    v => active().forEach((i, k, a) => macroWrite(detune[i], a.length < 2 ? 0 : v * (2 * k / (a.length - 1) - 1))));
  const STACKS = [['Unison', [0, 0, 0, 0]], ['Sub octave', [0, -12, 0, -12]], ['Octaves', [0, 12, -12, 24]], ['Fifth', [0, 7, 0, 7]], ['Fifth + sub', [0, 7, -12, -5]]];
  const stack = derived('stack', 'Stack: the pitched blocks that are on, set apart by octaves or fifths', 0, STACKS.length, [...sw.slice(0, 4), ...offset],
    () => { const n = STACKS.findIndex(s => active().every((i, k) => now(offset[i]) === s[1][k] * 512)); return n < 0 ? STACKS.length : n; },
    n => { if (n < STACKS.length) active().forEach((i, k) => macroWrite(offset[i], STACKS[n][1][k] * 512)); });

  // --- envelopes
  const SETS = [['amp', 'filter', 'tf'], ['amp'], ['filter', 'tf'], ['pitch']];
  const times = (fields, kinds) => kinds.flatMap(k => fields.flatMap(f => k === 'tf' ? [tf('FENV' + f)] : each(LANES[k] + f)));
  const everything = ['amp', 'filter', 'tf', 'pitch'];
  const STAGES = [['attack', 'Attack', ['aT']], ['decay', 'Decay', ['dT']], ['release', 'Release', ['r1T', 'r2T']]];
  const stages = STAGES.map(([id, name, fields]) => [trim(id, `${name} time of the envelopes chosen beside, from where each one is`, 64, times(fields, everything), ...add), name, fields]);
  const aimStages = () => { for (const [ref, , fields] of stages) ref.aim(times(fields, SETS[now(lane)])); };
  aimStages();

  // --- play
  const portaSw = each('tssOSCPortaSw'), portaTm = each('tssOSCPortaTm'), legatoSw = each('tssOSCLegatoSw');
  const glide = derived('glide', 'Glide: portamento of all blocks (0 switches it off)', 0, 127, [...portaSw, ...portaTm],
    () => { const on = BLOCKS.filter(i => now(portaSw[i]) === 1); return on.length ? Math.max(1, now(portaTm[on[0]])) : 0; },
    v => BLOCKS.forEach(i => { macroWrite(portaSw[i], v ? 1 : 0); if (v) macroWrite(portaTm[i], v); }));
  const legato = derived('legato', 'Legato on all blocks', 0, 1, legatoSw,
    () => legatoSw.every(r => now(r) === 1) ? 1 : 0, v => legatoSw.forEach(r => macroWrite(r, v)));
  const ampVelocityTargets = each('tssOSCAtch');
  const filterVelocityTargets = [...each('tssOSCFtch'), tf('Ftch')];
  const velocityTargets = [...ampVelocityTargets, ...filterVelocityTargets];
  const velocity = trim('velocity', 'Velocity: touch sense of every amp and filter, from where each one is', 64,
    velocityTargets, ...add);
  const follow = trim('follow', 'Key follow of every amp and filter, from where each one is', 128,
    [...each('tssOSCAkeyf'), ...each('tssOSCFkeyf'), tf('Fkeyf')], ...add);

  // --- tone
  const coarse = t => t.id === 'tssOSCFcoff' ? 8 : 1;      // a block's filter has 16 steps, the Total Filter 128
  const bright = trim('bright', 'Brightness: the Total Filter cutoff and the cutoff of every block', 64, [tf('Fcoff'), ...each('tssOSCFcoff')],
    (b, m, t) => b + m / coarse(t), (v, m, t) => v - m / coarse(t));
  const envAmount = trim('envamt', 'Filter envelope depth, Total Filter and blocks, from where each one is', 64, [tf('FEdep'), ...each('tssOSCFEdep')], ...add);
  const mix = trim('mix', 'Level of all blocks together, their balance kept', 100, each('tssOSCAlvl'),
    (b, m) => b * (1 + m / 100), (v, m, t, old) => m <= -100 ? (old ?? v) : v / (1 + m / 100));

  // --- LFO
  const depths = n => [each(`tssOSCPlfo${n}D`, PITCHED), [...each(`tssOSCFlfo${n}D`), tf(`Flfo${n}D`)], each(`tssOSCAlfo${n}D`)];
  const WOBBLES = [['vibrato', 'Vibrato', 'pitch'], ['wobble', 'Wobble', 'filter'], ['tremolo', 'Tremolo', 'amp']];
  const wobbles = WOBBLES.map(([id, name, what], k) => [trim(id, `${name}: depth of the chosen LFO on every ${what}, from where each one is`, 64,
    [...depths(1)[k], ...depths(2)[k]], ...add), name]);
  const aimWobbles = () => wobbles.forEach(([ref], k) => ref.aim(depths(now(lfo) + 1)[k]));
  aimWobbles();
  const clocks = [...Object.values(CLOCKS).flatMap(id => each(id)), tf('FEclk')], lfoClocks = each('tssLFOclk', [0, 1]);
  const DIVISIONS = [...D.enums.clockTrigger, 'Mixed'];
  const tempo = derived('tempo', 'Clock trigger of every envelope (and the LFOs’ division)', 0, DIVISIONS.length - 1, clocks,
    () => clocks.every(r => now(r) === now(clocks[0])) ? now(clocks[0]) : DIVISIONS.length - 1,
    n => { if (n < DIVISIONS.length - 1) { clocks.forEach(r => macroWrite(r, n)); if (n) lfoClocks.forEach(r => macroWrite(r, n - 1)); } });

  // --- the whole tone: independent morph and mutate tools
  const ends = [null, null];
  let refreshEndButtons = () => {}, refreshConfigControls = () => {}, loadMacroSettings = () => {};
  captureUndoExtra = () => ({ ends: ends.map(end => end && new Map(end)), morph: now(morph),
    editTarget: editTarget === 'both' ? null : editTarget,
    settings: Object.fromEntries(Object.entries(saved).filter(([id]) => id !== 'open' && id !== 'cc')),
    controllers: { ...(saved.cc || {}) } });
  restoreUndoExtra = state => {
    if (!state) return;
    ends.splice(0, ends.length, ...state.ends.map(end => end && new Map(end)));
    editTarget = state.editTarget === 'A' && ends[0] ? 'A' : state.editTarget === 'B' && ends[1] ? 'B' : 'both';
    loadMacroSettings(state.settings, state.controllers);
    store(morph, state.morph ?? null);
    refreshEndButtons();
    syncMacroLfo();
  };
  const morph = virt('morph', 'Morph between snapshots A and B, including their oscillator samples', 0, 100);
  const smooth = r => !r.labels && r.vt !== 'wf' && r.max - r.min > 2;
  let editTarget = 'both', targetButtons = [], applyingMorph = false, applyingConfig = false;
  const targetIndices = () => editTarget === 'A' ? [0] : editTarget === 'B' ? [1]
    : ends[0] && ends[1] ? [0, 1] : [];
  const morphPosition = () => clamp(now(morph) ?? 0, 0, 100) / 100;
  const valueAt = r => {
    const a = ends[0]?.get(r.key), b = ends[1]?.get(r.key), p = morphPosition();
    if (a != null && b != null) return smooth(r) ? a + (b - a) * p : p < 0.5 ? a : b;
    return a ?? b ?? null;
  };
  const updateEndpoints = (r, value, before) => {
    if (before == null) return false;
    const indices = targetIndices().filter(i => ends[i]?.has(r.key));
    if (!indices.length) return false;
    const delta = value - before, p = morphPosition();
    for (const i of indices) {
      const end = ends[i], saved = end.get(r.key);
      let change = delta;
      if (editTarget !== 'both' && smooth(r) && ends[0] && ends[1]) {
        const weight = i === 0 ? 1 - p : p;
        if (weight > 0) change /= weight;
      }
      end.set(r.key, smooth(r) ? clamp(saved + change, r.min, r.max) : value);
    }
    syncMacroLfo();
    return true;
  };
  let macroEdited = -Infinity;
  macroWrite = (r, value) => {
    value = clamp(Math.round(value), r.min, r.max);
    const before = now(r), hasEndpoint = targetIndices().some(i => ends[i]?.has(r.key));
    if (before != null && value !== before && hasEndpoint) {
      if (performance.now() - macroEdited > 1000) remember();
      macroEdited = performance.now();
      updateEndpoints(r, value, before);
      editLater(r, valueAt(r) ?? value);
    } else editLater(r, value);
  };
  setVelocityValuesAction = (engine, updates) => {
    if (!updates?.length) return false;
    if (engine === 'solo') {
      for (const { ref, value } of updates) {
        targetIndices().forEach(i => { if (ends[i]?.has(ref.key)) ends[i].set(ref.key, value); });
        editLater(ref, value);
      }
      velocity.reset();
      syncMacroLfo();
    } else updates.forEach(({ ref, value }) => editLater(ref, value));
    return true;
  };
  const recordEndpointEdit = (r, value, before) => {
    if (applyingMorph || r.ct !== 9 || before == null || value === before) return;
    const hasEndpoint = targetIndices().some(i => ends[i]?.has(r.key));
    if (!hasEndpoint) return;
    macroEdited = -Infinity;
    updateEndpoints(r, value, before);
    const after = valueAt(r);
    if (after != null && after !== value) editLater(r, after);
  };
  recordLinkedEndpointEdit = recordEndpointEdit;
  let morphed = 0;
  morph.set = v => {
    if (!ends[0] || !ends[1]) return;
    if (!applyingConfig && performance.now() - morphed > 1000) remember();      // once for a turn of the knob
    morphed = performance.now();
    v = clamp(Math.round(v), 0, 100);
    const blendWaves = wavesReady && v > 0 && v < 100;
    applyingMorph = true;
    try {
      for (const [key, a] of ends[0]) {
        const b = ends[1].get(key), r = refs.get(key);
        if (b != null) {
          if (blendWaves && r.id === 'tssOSCwf' && r.inst < 5) edit(r, a);
          else editLater(r, smooth(r) ? a + (b - a) * v / 100 : v < 50 ? a : b);
        }
      }
    } finally { applyingMorph = false; }
    const command = ++waveCommand;
    if (blendWaves) {
      const a = [], b = [];
      for (let i = 0; i < 5; i++) {
        const key = solo('tssOSCwf', i).key;
        a.push(ends[0].get(key)); b.push(ends[1].get(key));
      }
      if (a.every(x => x != null) && b.every(x => x != null)) {
        setTimeout(() => { if (command === waveCommand && linked) ws.send(`w ${v} ${a.join(' ')} ${b.join(' ')}`); }, 120);
      }
    } else if (linked) ws.send('w off');
    store(morph, v);
  };
  editHooks.push((r, v) => {
    if (applyingMorph || r.ct !== 9) return;
    const before = valueAt(r), hasEndpoint = targetIndices().some(i => ends[i]?.has(r.key));
    if (!hasEndpoint) return;
    recordEndpointEdit(r, v, before);
    if (r.id === 'tssOSCwf' && now(morph) > 0 && now(morph) < 100) morph.set(now(morph));
  });
  const editHint = el('span', { class: 'morph-hint' });
  const refreshEditTargets = () => {
    targetButtons.forEach((b, i) => {
      const target = i ? 'B' : 'A';
      b.disabled = !ends[i];
      b.classList.toggle('on', editTarget === target);
      b.title = ends[i] ? `Edit snapshot ${target}. This previews that endpoint and sends parameter and macro edits to it. Click again to clear the target.`
        : `Store snapshot ${target} before editing it directly.`;
    });
    editHint.textContent = editTarget !== 'both' ? `Editing ${editTarget}: parameter and macro edits save there. Click Edit ${editTarget} again to clear.`
      : ends[0] && ends[1] ? 'No endpoint selected: edits adjust A and B together.'
        : ends[0] || ends[1] ? 'The stored snapshot stays fixed while edits change the live tone. Store the other snapshot when ready.'
          : 'Store A or B before choosing an edit target.';
  };
  const chooseEditTarget = target => {
    const next = editTarget === target ? 'both' : target;
    if (next !== editTarget) macroEdited = -Infinity;
    editTarget = next;
    refreshEditTargets();
    refreshEndButtons();
    const index = editTarget === 'A' ? 0 : editTarget === 'B' ? 1 : -1;
    if (index < 0 || !ends[index]) return;
    if (ends[0] && ends[1]) morph.set(index * 100);
    else for (const [key, value] of ends[index]) editLater(refs.get(key), value);
  };
  targetButtons = ['A', 'B'].map((target, i) => {
    const b = el('button', { class: 'pill end-target', text: 'Edit ' + target });
    b.addEventListener('click', () => chooseEditTarget(target));
    return b;
  });
  refreshEditTargets();
  const endButtons = ['A', 'B'].map((name, n) => {
    const b = el('button', { class: 'pill end', text: 'Store ' + name });
    const show = () => {
      b.textContent = ends[n] ? `Clear ${name}` : `Store ${name}`;
      b.classList.toggle('on', !!ends[n]);
      b.title = ends[n] ? `Snapshot ${name} is kept.${editTarget === name ? ' It is selected for editing.' : ''} Click to clear it.`
        : `Keep the whole tone as it is now as snapshot ${name}. With A and B kept, Morph blends between them.`;
    };
    show();
    // lit: a snapshot is kept, and a click drops it
    b.addEventListener('click', () => {
      if (!ends[n] && now(sw[0]) == null) return;
      const dropped = !!ends[n];
      ends[n] = dropped ? null : snapshot();
      if (dropped && editTarget === name) editTarget = 'both';
      if (!ends[0] || !ends[1]) { waveCommand++; if (linked) ws.send('w off'); }
      refreshEndButtons();
      store(morph, ends[0] && ends[1] ? 100 * n : null);
      syncMacroLfo();
    });
    return b;
  });
  refreshEndButtons = () => {
    endButtons.forEach((b, n) => {
      const name = n ? 'B' : 'A';
      b.textContent = ends[n] ? `Clear ${name}` : `Store ${name}`;
      b.classList.toggle('on', !!ends[n]);
      b.title = ends[n] ? `Snapshot ${name} is kept.${editTarget === name ? ' It is selected for editing.' : ''} Click to clear it.`
        : `Keep the whole tone as it is now as snapshot ${name}. With A and B kept, Morph blends between them.`;
    });
    refreshEditTargets();
    refreshConfigControls();
  };

  // The Macro LFO sends its two tone states to the engine. The engine samples
  // the oscillator per allocated voice; the browser never turns a live knob
  // for it. Only mapped Solo parameters can be changed in this path.
  const mapped = new Set((cells || []).filter(c => c[0] === 9).map(c => `${c[1]}:${c[2]}:${c[3]}`));
  const wire = (r, v) => encode(r, clamp(Math.round(v), r.min, r.max)).reduce((n, b, i) => n + b * 128 ** i, 0);
  const row = (r, a, b, step = false) => r && now(r) != null && mapped.has(`${r.pid}:${r.inst}:${r.ai}`)
    ? [r.pid, r.inst, r.ai, wire(r, a), wire(r, b), wire(r, now(r)), step ? 1 : 0] : null;
  const around = (list, span, scale = () => 1) => list.map(r => row(r, now(r) - span * scale(r), now(r) + span * scale(r))).filter(Boolean);
  const rateHz = () => 0.02 * 2 ** (now(macroLfoRate) / 10);
  let lfoTimer = null;
  syncMacroLfo = () => {
    clearTimeout(lfoTimer);
    lfoTimer = setTimeout(() => {
      if (!linked) return;
      const target = now(macroLfoTarget);
      if (!target) { sendMidi([0xF0, 0x7D, 0x58, 0x4D, 0xF7]); return; }
      let points = [], wave = null;
      if (target === 1 && ends[0] && ends[1]) {
        if (wavesReady) {
          const a = [], b = [];
          for (let i = 0; i < 5; i++) {
            const key = solo('tssOSCwf', i).key;
            a.push(ends[0].get(key)); b.push(ends[1].get(key));
          }
          if (a.every(x => x != null) && b.every(x => x != null)) wave = { a, b };
        }
        for (const [key, a] of ends[0]) {
          const b = ends[1].get(key), r = refs.get(key);
          if (b == null || !r || r.ct !== 9) continue;
          const oscWave = wave && r.id === 'tssOSCwf' && r.inst < 5;
          const item = row(r, a, oscWave ? a : b, !smooth(r));
          if (item) points.push(item);
        }
      } else if (target === 2) points = around([tf('Fcoff'), ...each('tssOSCFcoff')], 64, r => 1 / coarse(r));
      else if (target === 3) points = around([tf('FEdep'), ...each('tssOSCFEdep')], 64);
      else if (target === 4) points = each('tssOSCAlvl').map(r => row(r, now(r) * 0.5, now(r) * 1.5)).filter(Boolean);
      else if (target === 5) points = detune.map((r, i) => row(r, 0, i < 4 ? (2 * i / 3 - 1) * 255 : 0)).filter(Boolean);
      else if (target === 6) points = [...portaSw.map(r => row(r, 0, 1, true)), ...portaTm.map(r => row(r, 0, 127))].filter(Boolean);
      else if (target === 7) points = around([...each('tssOSCAtch'), ...each('tssOSCFtch'), tf('Ftch')], 64);
      else if (target === 8) points = around([...each('tssOSCAkeyf'), ...each('tssOSCFkeyf'), tf('Fkeyf')], 128);
      else if (target >= 9 && target <= 11) points = around(depths(now(lfo) + 1)[target - 9], 64);
      else if (target >= 12 && target <= 14) points = around(times(STAGES[target - 12][2], SETS[now(lane)]), 64);
      if (!points.length) { sendMidi([0xF0, 0x7D, 0x58, 0x4D, 0xF7]); return; }
      const config = { mode: ['hold', 'free', 'trig', 'one', 'half'][now(macroLfoMode)],
        shape: ['sine', 'triangle', 'saw', 'ramp', 'exp', 'square', 'random'][now(macroLfoShape)],
        rate: rateHz(), depth: now(macroLfoDepth), center: now(macroLfoCenter), points, wave };
      sendMidi([0xF0, 0x7D, 0x58, 0x4D, ...Array.from(JSON.stringify(config), c => c.charCodeAt(0)), 0xF7]);
    }, 120);
  };
  for (const ref of [macroLfoTarget, macroLfoMode, macroLfoShape, macroLfoRate, macroLfoDepth, macroLfoCenter, lane, lfo])
    watch(ref, syncMacroLfo);
  editHooks.push(ref => { if (ref.ct === 9 && now(macroLfoTarget)) syncMacroLfo(); });
  keyWatch.push(syncMacroLfo);

  const configSelect = el('select', { class: 'morph-config-select', title: 'Choose a saved morph and macro configuration' },
    el('option', { value: '', text: 'Saved configs' }));
  const configName = el('input', { class: 'morph-config-name', type: 'text', maxlength: '40', placeholder: 'Name this setup', 'aria-label': 'Morph configuration name' });
  const configStatus = el('span', { class: 'morph-config-status' });
  const configSave = el('button', { class: 'pill config-action', text: 'Save', title: 'Save A/B snapshots, Morph position, macro settings and the current Solo Synth preset' });
  const configLoad = el('button', { class: 'pill config-action', text: 'Load', title: 'Load the selected morph and macro configuration with its Solo Synth preset', disabled: '' });
  const configDelete = el('button', { class: 'pill config-action', text: 'Delete', title: 'Delete the selected saved configuration', disabled: '' });
  const configControls = el('div', { class: 'morph-configs' }, configSelect, configName, configSave, configLoad, configDelete);
  const configWrap = el('div', { class: 'morph-config-wrap' }, configControls, configStatus);
  let loadingConfig = false;
  refreshConfigControls = () => {
    configSelect.disabled = loadingConfig;
    configName.disabled = loadingConfig;
    configSave.disabled = loadingConfig || !(ends[0] || ends[1]) || !(configName.value.trim() || configSelect.value);
    configLoad.disabled = loadingConfig || !configSelect.value || !morphConfigs[configSelect.value];
    configDelete.disabled = loadingConfig || !configSelect.value || !morphConfigs[configSelect.value];
  };
  const refreshConfigList = selected => {
    configSelect.replaceChildren(el('option', { value: '', text: 'Saved configs' }),
      ...Object.keys(morphConfigs).sort((a, b) => a.localeCompare(b)).map(name => el('option', { value: name, text: name })));
    configSelect.value = selected && morphConfigs[selected] ? selected : '';
    configName.value = configSelect.value;
    refreshConfigControls();
  };
  loadMacroSettings = (settings = {}, controllers = {}) => {
    for (const key of Object.keys(saved)) if (key !== 'open' && key !== 'cc') delete saved[key];
    for (const [id, ref] of byId) if (ref.setting && id !== 'open') {
      const value = Number.isFinite(Number(settings[id])) ? Number(settings[id]) : ref.defaultValue;
      ref.set(value);
    }
    for (const [id, value] of Object.entries(settings))
      if (!byId.has(id) && id.startsWith('free.') && Number.isFinite(Number(value))) saved[id] = Number(value);
    saved.cc = Object.fromEntries(Object.entries(controllers).filter(([id, cc]) => typeof id === 'string' && Number.isInteger(Number(cc)) && Number(cc) >= 1 && Number(cc) <= 119));
    keep();
    tags();
  };
  configName.addEventListener('input', () => { configStatus.textContent = ''; refreshConfigControls(); });
  configSelect.addEventListener('change', () => {
    configName.value = configSelect.value;
    configStatus.textContent = '';
    refreshConfigControls();
  });
  configSave.addEventListener('click', () => {
    const name = configName.value.trim() || configSelect.value;
    if (!name || !(ends[0] || ends[1])) return;
    if (['__proto__', 'constructor', 'prototype'].includes(name)) { configStatus.textContent = 'Choose a different name.'; return; }
    const previous = morphConfigs[name];
    const settings = { ...saved };
    delete settings.open;
    delete settings.cc;
    morphConfigs[name] = {
      version: 2, preset: window.soloPreset?.current?.() ?? null,
      ends: ends.map(end => end && [...end]), morph: now(morph),
      editTarget: editTarget === 'both' ? null : editTarget, settings,
      controllers: { ...(saved.cc || {}) }
    };
    if (!keepMorphConfigs()) {
      if (previous) morphConfigs[name] = previous; else delete morphConfigs[name];
      configStatus.textContent = 'Could not save in browser storage.';
      refreshConfigControls();
      return;
    }
    refreshConfigList(name);
    configStatus.textContent = `Saved “${name}”.`;
  });
  configLoad.addEventListener('click', async () => {
    const name = configSelect.value, config = morphConfigs[name];
    if (!config || !Array.isArray(config.ends)) return;
    const loadedEnds = [0, 1].map(i => {
      const rows = config.ends[i];
      if (!Array.isArray(rows)) return null;
      const values = rows.filter(row => Array.isArray(row) && row.length === 2 && refs.get(row[0])?.ct === 9 && Number.isFinite(Number(row[1])))
        .map(([key, value]) => [key, clamp(Math.round(Number(value)), refs.get(key).min, refs.get(key).max)]);
      return values.length ? new Map(values) : null;
    });
    if (!loadedEnds[0] && !loadedEnds[1]) { configStatus.textContent = 'This configuration has no usable snapshots.'; return; }
    const hasPreset = config.preset != null;
    const preset = hasPreset ? Number(config.preset) : null;
    if (hasPreset && !Number.isInteger(preset)) { configStatus.textContent = 'This configuration has an invalid synth preset.'; return; }
    if (hasPreset && !window.soloPreset?.load) { configStatus.textContent = 'Solo preset loading is unavailable.'; return; }
    const changesPreset = hasPreset && window.soloPreset.current() !== preset;
    loadingConfig = true;
    refreshConfigControls();
    configStatus.textContent = changesPreset ? 'Loading synth preset…' : 'Loading configuration…';
    try {
      if (changesPreset) {
        // A preset change is not part of the tone editor's single-step Undo.
        undo = null;
        undoButton.disabled = true;
        await window.soloPreset.load(preset);
      } else remember();
      applyingConfig = true;
      try {
        const settings = config.settings && typeof config.settings === 'object' ? config.settings : {};
        const controllers = config.controllers && typeof config.controllers === 'object' ? config.controllers : {};
        loadMacroSettings(settings, controllers);
        ends.splice(0, ends.length, ...loadedEnds);
        editTarget = config.editTarget === 'A' && ends[0] ? 'A' : config.editTarget === 'B' && ends[1] ? 'B' : 'both';
        refreshEndButtons();
        const position = Number.isFinite(Number(config.morph)) ? clamp(Math.round(Number(config.morph)), 0, 100) : 0;
        if (ends[0] && ends[1]) morph.set(position);
        else {
          waveCommand++;
          if (linked) ws.send('w off');
          const end = ends[0] || ends[1];
          for (const [key, value] of end) editLater(refs.get(key), value);
          store(morph, null);
        }
        macroEdited = -Infinity;
        morphed = performance.now();
        syncMacroLfo();
      } finally { applyingConfig = false; }
      const presetName = hasPreset ? ` with ${window.soloPreset.label(preset)}` : '';
      configStatus.textContent = `Loaded “${name}”${presetName}.`;
    } catch (error) {
      configStatus.textContent = error?.message || 'Could not load this configuration.';
    } finally {
      loadingConfig = false;
      refreshConfigControls();
    }
  });
  configDelete.addEventListener('click', () => {
    const name = configSelect.value;
    if (!morphConfigs[name] || !confirm(`Delete the saved morph configuration “${name}”?`)) return;
    const previous = morphConfigs[name];
    delete morphConfigs[name];
    if (!keepMorphConfigs()) {
      morphConfigs[name] = previous;
      refreshConfigList(name);
      configStatus.textContent = 'Could not update browser storage.';
      return;
    }
    refreshConfigList();
    configStatus.textContent = `Deleted “${name}”.`;
  });
  refreshConfigList();

  const NEVER = /sw$|Sw$|wf$|Poset$|Pkeyf$|keyfB$|clk$|sync$|type$|gain$|rtrg$|PortaTm$|^tssOSCX/;
  const mutate = el('button', { class: 'pill', text: 'Mutate', title: 'Move the tone’s values at random, by up to the amount beside' });
  mutate.addEventListener('click', () => {
    if (now(sw[0]) == null) return;
    remember();
    for (const r of refs.values()) {
      if (r.ct !== 9 || NEVER.test(r.id) || now(r) == null) continue;
      const p = P[r.id], part = p.block === 'OSC' ? p.group : p.block === 'PWM' ? 'Pitch' : p.block;
      if (!locks[part] || now(locks[part]) === 1 || (p.count === 6 && now(sw[r.inst]) !== 1)) continue;
      macroWrite(r, now(r) + (Math.random() * 2 - 1) * now(amount) / 100 * (r.max - r.min));
    }
  });
  const keepButton = el('button', { class: 'chip', text: 'Keep', title: 'What Mutate leaves alone' });
  keepButton.addEventListener('click', e => {
    e.stopPropagation();
    popover.replaceChildren(el('div', { class: 'head', text: 'Mutate leaves alone' }),
      ...LOCKS.map(([part, label]) => el('div', { class: 'row' }, led(locks[part], label, locks[part].set), label)),
      el('div', { class: 'foot', text: 'Switches, waves, tuning, glide and clock settings are never touched, nor are blocks that are off.' }));
    popover.hidden = false;
    popover.style.width = '';
    const r = keepButton.getBoundingClientRect();
    popover.style.left = clamp(r.left, 12, innerWidth - popover.offsetWidth - 12) + 'px';
    popover.style.top = Math.max(12, r.top - popover.offsetHeight - 6) + 'px';
  });
  // a new tone: nothing to take back (the snapshots stay, so that two presets can be morphed)
  watch(sw[0], v => { if (v == null) { undo = null; undoButton.disabled = true; } });

  // --- voice variation: with several voices, every voice but the first holds these a little off (the player does
  // it: `b vglide N` ...; nothing of it is in the tone). Shown while there is more than one voice.
  const VARY = [['glide', 'Glide', 'Glide: each voice slides at its own speed (with Glide on in the MIDI menu)'],
    ['filter', 'Filters', 'Filters: each voice a little brighter or darker (Total Filter cutoff)'],
    ['env', 'Envelopes', 'Envelopes: each voice a little faster or slower (amp and Total Filter envelope times)'],
    ['level', 'Levels', 'Levels: each voice a little louder or softer']];
  const vary = VARY.map(([id, , title], n) => {
    const ref = virt('vary-' + id, title, 0, 127);
    let sent = 0, last = null, moved = 0;
    ref.set = v => {
      moved = Date.now();
      // the player answers every one: at most 25 a second, the last one always
      last = clamp(Math.round(v), 0, 127);
      store(ref, last);
      if (sent) return;
      sent = setTimeout(() => { sent = 0; if (poly.vary[n] !== last) ws.send(`b v${id} ${last}`); }, 40);
    };
    ref.busy = () => Date.now() < moved + 300;   // an answer to an older value does not pull the knob back
    return ref;
  });
  let varyPart = null;
  const variation = () => {
    vary.forEach((ref, n) => { if (!ref.busy() && now(ref) !== poly.vary[n]) store(ref, poly.vary[n]); });
    if (!varyPart) return;
    varyPart.hidden = poly.mode === 'multi' || poly.voices < 2;
    varyPart.firstElementChild.nextElementSibling.firstElementChild.classList.toggle('idle', !poly.glide);
  };
  keyWatch.push(variation);

  // --- the page
  const signed = v => v == null ? '–' : (v > 0 ? '+' : '') + v;
  const k = (ref, label, o = {}) => register(ref, knob(ref, { label, size: 'sm', set: ref.set, reset: 0, ...o }));
  const part = (cls, title, ...kids) => el('div', { class: 'mpart ' + cls }, el('h3', { text: title }), el('div', { class: 'mrow' }, kids));
  const col = (...kids) => el('div', { class: 'mcol' }, kids);
  const sw1 = (ref, text, title) => field(text, led(ref, title, ref.set));
  const fold = el('button', { class: 'fold', text: 'Macros', title: 'Show or hide the macros' });
  fold.addEventListener('click', () => open.set(now(open) === 1 ? 0 : 1));
  watch(open, v => { $('#macros').classList.toggle('shut', v !== 1); fold.setAttribute('aria-expanded', v === 1); });
  $('#macros').append(
    el('h2', {}, fold, el('em', { text: 'one control, many parameters · the tone keeps only the result' }),
       el('span', { class: 'right' }, undoButton, learnButton)),
    el('div', { class: 'mparts' },
      el('div', { class: 'mline' },
        part('pitch', 'Tune', k(spread, 'Spread'),
          col(pick(stack, [...STACKS.map(s => s[0]), 'Custom'], { label: 'Stack', number: false, set: stack.set }))),
        part('ampl', 'Envelopes', stages.map(([ref, name]) => k(ref, name)),
          col(seg(lane, ['Amp + Flt', 'Amp', 'Filter', 'Pitch'], v => { lane.set(v); aimStages(); }),
              el('div', { class: 'fields' }, seg(adsr, ['Full', 'ADSR'], adsr.set), el('span', { class: 'field', text: 'Link' }),
                 sw1(link.pitch, 'Pitch', 'An edit to one block’s pitch envelope goes to all blocks'),
                 sw1(link.filter, 'Filter', 'An edit to one block’s filter envelope goes to all blocks'),
                 sw1(link.amp, 'Amp', 'An edit to one block’s amp envelope goes to all blocks')))),
        part('filt', 'Tone', k(bright, 'Bright'), k(envAmount, 'Env amt'), k(mix, 'Mix', { text: v => v == null ? '–' : signed(v) + '%' }))),
      el('div', { class: 'mline' },
        part('gold', 'Play', k(glide, 'Glide', { text: v => v == null ? '–' : v ? String(v) : 'off' }), k(velocity, 'Velocity'), k(follow, 'Key flw'),
          col(sw1(legato, 'Legato', 'Legato on all blocks'))),
        part('lfo', 'LFO', wobbles.map(([ref, name]) => k(ref, name)),
          col(seg(lfo, ['LFO 1', 'LFO 2'], v => { lfo.set(v); aimWobbles(); }),
              pick(tempo, DIVISIONS, { label: 'Clock', number: false, set: tempo.set }))),
        part('fx morph', 'Morph', col(el('div', { class: 'morph-actions' },
            el('div', { class: 'fields morph-snapshots' }, endButtons), el('div', { class: 'fields morph-target' }, targetButtons)), editHint, configWrap),
          k(morph, 'Morph', { size: 'big', text: v => v == null ? '–' : v === 0 ? 'A' : v === 100 ? 'B' : v + '%' })),
        part('macro-lfo', 'Macro LFO',
          col(pick(macroLfoTarget, LFO_TARGETS, { label: 'Destination', set: macroLfoTarget.set }),
              pick(macroLfoMode, ['Hold', 'Free', 'Trig', 'One', 'Half'], { label: 'Mode', set: macroLfoMode.set }),
              pick(macroLfoShape, ['Sine', 'Triangle', 'Saw', 'Ramp', 'Exp', 'Square', 'Random'], { label: 'Wave', set: macroLfoShape.set })),
          knob(macroLfoRate, { label: 'Rate', size: 'sm', set: macroLfoRate.set, text: () => rateHz().toFixed(2) + ' Hz' }),
          knob(macroLfoDepth, { label: 'Depth', size: 'sm', set: macroLfoDepth.set, text: v => v + '%' }),
          knob(macroLfoCenter, { label: 'Center', size: 'sm', set: macroLfoCenter.set, text: v => v + '%' })),
        part('fx', 'Mutate',
          knob(amount, { label: 'Amount', size: 'sm', set: amount.set, reset: 15, text: v => v + '%' }),
          col(mutate, keepButton)),
        varyPart = part('voices', 'Voice variation', vary.map((ref, n) => k(ref, VARY[n][1]))))));
  variation();
  tags();
}

window.macros = { adsr, envelopeTools, blockButton, controller, build, remember,
  setVelocityValues: (engine, updates) => setVelocityValuesAction(engine, updates),
  syncLfo: () => syncMacroLfo(),
  waveReady: ready => { if (wavesReady === ready) return; wavesReady = ready; if (ready && byId.get('morph') && now(byId.get('morph')) > 0 && now(byId.get('morph')) < 100) byId.get('morph').set(now(byId.get('morph'))); syncMacroLfo(); },
  ref: id => byId.get(id) };
})();
