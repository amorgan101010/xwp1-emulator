// Drawbar Organ page. Drawbar positions use CCs and are confirmed by SysEx readback.
'use strict';

let drawData;
let drawProgram = null;
const drawRefs = {};
const drawVals = {};
window.drawData = null;
window.drawVals = drawVals;
window.drawProgram = drawProgram;

function drawRef(id, instance = 0) {
  const p = drawData.params.find(x => x.id === id);
  if (!p) throw new Error(`Unknown Drawbar parameter: ${id}`);
  if (instance < 0 || instance >= p.count) throw new RangeError(`Drawbar index must be 0..${p.count - 1}`);
  const inst = p.count === 1 ? 0 : instance;
  const key = `${id}/${inst}`;
  if (drawRefs[key]) return drawRefs[key];
  const ref = makeRef(7, p.pid, inst, 0, {
    vt: 'nf', min: p.min, max: p.max, name: p.name, id, count: p.count
  });
  drawRefs[key] = ref;
  watch(ref, v => { if (v == null) delete drawVals[key]; else drawVals[key] = v; });
  return ref;
}

function drawEdit(id, instance, raw) {
  if (arguments.length === 2) { raw = instance; instance = 0; }
  const ref = drawRef(id, instance);
  if (id === 'organPosition') {
    drawPosition(instance, raw);
    return;
  }
  edit(ref, raw);
}
window.drawEdit = drawEdit;

// A drawbar goes out as its controller (the instrument renders the organ wave only then). The
// page shows the new position at once and reads it back 300 ms after the last move: the
// firmware applies the controller a little later than it answers a request.
const drawReadback = new Map();
function drawPosition(instance, raw) {
  const ref = drawRef('organPosition', instance);
  const position = clamp(Math.round(raw), 0, 8);
  if (vals.get(ref.key) == null || vals.get(ref.key) === position) return;
  const flight = inflight.get(ref.key);
  if (flight) flight.ignore = true;
  queue = queue.filter(r => r !== ref);
  store(ref, position);
  sendMidi([0xB0, drawData.barCC[instance], drawData.positionCC[position]]);
  clearTimeout(drawReadback.get(ref.key));
  drawReadback.set(ref.key, setTimeout(() => {
    queue = queue.filter(r => r !== ref);
    queue.unshift(ref); total++; pump();
  }, 300));
}

const DRAW_PAD = 18;     // px between the well's edge and positions 0 / 8
const drawFootage = ["16'", "5 1/3'", "8'", "4'", "2 2/3'", "2'", "1 3/5'", "1 1/3'", "1'"];
const drawCap = ['brown', 'brown', 'white', 'white', 'black', 'white', 'black', 'black', 'white'];

function drawbarControl(instance, footage, cap) {
  const ref = drawRef('organPosition', instance);
  const well = el('div', { class: `drawbar-well ${cap}`, role: 'slider', tabindex: 0,
    'aria-label': `${footage} drawbar`, 'aria-valuemin': 0, 'aria-valuemax': 8 });
  const rail = el('div', { class: 'drawbar-rail' });
  // position p sits at DRAW_PAD + p / 8 of the travel: numbers, shaft and grip all use this
  const at = p => `calc(${DRAW_PAD}px + ${p / 8} * (100% - ${2 * DRAW_PAD}px))`;
  const marks = el('div', { class: 'drawbar-marks' }, ...Array.from({ length: 9 }, (_, i) => el('b', { text: String(i), style: `top:${at(i)}` })));
  const shaft = el('div', { class: 'drawbar-shaft' });
  const grip = el('div', { class: 'drawbar-grip' });
  const label = el('label', { text: footage });
  const value = el('output', { text: '–' });
  well.append(rail, marks, shaft, grip);
  const paint = v => {
    well.classList.toggle('wait', v == null);
    const pos = v == null ? 8 : clamp(v, 0, 8);
    grip.style.top = at(pos);
    shaft.style.height = at(pos);
    marks.querySelectorAll('b').forEach((b, i) => b.classList.toggle('out', v != null && i <= pos));
    well.setAttribute('aria-valuenow', v ?? '');
    value.textContent = v == null ? '–' : String(v);
  };
  watch(ref, paint);
  let start = null;
  const fromPoint = y => {
    const r = well.getBoundingClientRect();
    drawEdit('organPosition', instance, Math.round(clamp((y - r.top - DRAW_PAD) / (r.height - 2 * DRAW_PAD), 0, 1) * 8));
  };
  well.addEventListener('pointerdown', e => {
    well.setPointerCapture(e.pointerId);
    start = true;
    fromPoint(e.clientY);
    e.preventDefault();
  });
  well.addEventListener('pointermove', e => { if (start) fromPoint(e.clientY); });
  const end = () => { start = false; };
  well.addEventListener('pointerup', end);
  well.addEventListener('pointercancel', end);
  well.addEventListener('keydown', e => {
    const step = { ArrowDown: 1, ArrowUp: -1, PageDown: 4, PageUp: -4 }[e.key];
    if (step != null) { e.preventDefault(); drawEdit('organPosition', instance, (drawVals[`organPosition/${instance}`] ?? 0) + step); }
  });
  return el('div', { class: 'drawbar' }, label, well, value);
}

function drawKnob(id, label, size = '') {
  return knob(drawRef(id), { label, size });
}
function buildDrawPage() {
  const labels = drawData.params.find(p => p.id === 'organPosition').labels;
  const order = drawFootage.map(label => labels.indexOf(label));
  if (order.some(instance => instance < 0)) throw new Error('Drawbar footage labels do not match drawbar.json');
  const bars = el('div', { class: 'drawbars' }, ...order.map((instance, i) => drawbarControl(instance, drawFootage[i], drawCap[i])));
  const ref = id => drawRef(id);
  const controls = el('section', { class: 'group draw-controls' },
    el('h2', {}, el('span', { text: 'Organ controls' }), el('em', { text: 'percussion, clicks and vibrato' })),
    el('div', { class: 'fields' }, field('Percussion', seg(ref('organPercussion'), ['Off', '2nd', '3rd', '2nd+3rd'])),
      field('Key-on click', led(ref('organKeyonClick'))), field('Key-off click', led(ref('organKeyoffClick'))),
      field('Rotary type', seg(ref('organRotaryType'), ['Type 1', 'Type 2']))),
    decayCurve(drawRef('organPercDecayTime'), drawRef('organPercussion'), 'percussion'),
    el('div', { class: 'ctls draw-knobs' }, drawKnob('organPercDecayTime', 'Perc. decay', 'big'),
      drawKnob('organVibratoRate', 'Vib rate'), drawKnob('organVibratoDepth', 'Vib depth')));
  $('#drawPage').replaceChildren(el('section', { class: 'group drawbar-panel' },
    el('h2', {}, el('span', { text: 'Drawbars' }), el('em', { text: 'pull down to raise each footage' })), bars,
    organWave(Array.from({ length: 9 }, (_, i) => drawRef('organPosition', i)), 'the wave these drawbars make')), controls);
}

function showDrawPreset() {
  if (document.body.matches('.perform-mode, .panel-mode')) return;
  if (drawProgram == null && window.showUserSlot?.()) return;
  $('#presetNum').textContent = drawProgram == null ? '---' : String(drawProgram).padStart(3, '0');
  $('#presetName').textContent = drawProgram == null ? 'Drawbar Organ tone' : drawData.presets[drawProgram];
}
function enterDraw(n) {
  window.pcmToneTarget = null;
  drawProgram = n; window.drawProgram = n;
  activeEngine = 'draw'; window.activeEngine = 'draw';
  document.body.classList.add('draw-mode');
  document.body.classList.remove('hex-mode', 'pcm-mode');
  $('#soloPage').hidden = true; $('#hexPage').hidden = true; $('#drawPage').hidden = false; $('#pcmPage').hidden = true;
  $('#fx').hidden = true; $('#engineFx').hidden = false;
  $('#soloTab').classList.remove('on'); $('#hexTab').classList.remove('on');
  $('#drawTab').classList.toggle('on', !document.body.matches('.perform-mode, .panel-mode')); $('#pcmTab').classList.remove('on');
  showDrawPreset();
  window.updateVelocityButtons?.();
}
window.enterDraw = enterDraw;
function chooseDrawPreset(n) {
  n = ((n % 50) + 50) % 50;
  enterDraw(n);
  releaseAll();
  discardToneEdits();
  sendMidi([0xB0, 0, 96, 0xB0, 0x20, 0, 0xC0, n]);
  for (const ref of refs.values()) if (ref.ct === 7 || ref.ct === 8 || ref.ct === 9) forget(ref);
  queue = []; inflight.clear();
  store(toneNumber(), 150 + n);         // the part's tone number: the program change does not send it back
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(() => readPatch(false), 450);
  showDrawPreset();
}
window.chooseDrawPreset = chooseDrawPreset;
window.initDraw = async function initDraw() {
  drawData = await (await fetch('drawbar.json')).json();
  window.drawData = drawData;
  for (const p of drawData.params) for (let i = 0; i < p.count; i++) drawRef(p.id, i);
  buildDrawPage();
  return drawData;
};
