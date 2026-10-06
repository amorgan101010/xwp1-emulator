// Hex Layer editor. References share app.js's SysEx socket, readback queue and controls.
'use strict';

let hexData;
let hexProgram = null;
let hexSelectedLayer = 0;
let hexOpenLayer = null;
const hexRefs = {};
const hexVals = {};
window.hexVals = hexVals;
window.hexProgram = hexProgram;
window.hexSelectedLayer = hexSelectedLayer;
window.hexData = null;

function hexRef(id, layer = 0) {
  const p = hexData.params.find(x => x.id === id);
  if (!p) throw new Error(`Unknown Hex Layer parameter: ${id}`);
  if (p.count === 6 && (layer < 0 || layer > 5)) throw new RangeError('Hex Layer index must be 0..5');
  const inst = p.count === 1 ? 0 : layer;
  const key = `${id}/${inst}`;
  if (hexRefs[key]) return hexRefs[key];
  const wf = id === 'hexWaveNumber';
  const max = wf ? 326 + hexData.waves.length - 1
    : id === 'hexVolumeOfs' ? 127
    : id === 'hexOnoff' || id === 'hexPitchLock' ? 1
    : id === 'hexDetuneNumber' ? 31
    : p.enum === 'hexLayerLfoWave' ? hexData.lfoWave.length - 1
    : p.bytes === 1 ? 127 : p.bytes === 2 ? 255 : 16383;
  const ref = makeRef(8, p.pid, inst, 0, {
    vt: wf ? 'hxwf' : 'hx', min: 0,
    max,
    name: p.name, id, count: p.count, bytes: p.bytes,
    labels: p.enum === 'hexLayerLfoWave' ? hexData.lfoWave : null,
    hexCenter: ['hexPanOffset', 'hexPitchKey'].includes(id) ? 64
      : ['hexAmpAttackOfs', 'hexAmpDecayOfs', 'hexAmpSustainOfs', 'hexAmpReleaseOfs', 'hexCutoffOfs', 'hexReverbSendOfs', 'hexChorusSendOfs', 'hexTouchSenseOfs', 'hexPitchAutoDepth', 'hexAmpAutoDepth'].includes(id) ? 128
      : ['hexPitchModDepth', 'hexPitchAfterDepth', 'hexAmpModDepth', 'hexAmpAfterDepth'].includes(id) ? 64 : null
  });
  hexRefs[key] = ref;
  watch(ref, v => { if (v == null) delete hexVals[key]; else hexVals[key] = v; });
  return ref;
}

function hexEdit(id, layer, raw) {
  if (arguments.length === 2) { raw = layer; layer = 0; }
  const p = hexData.params.find(x => x.id === id);
  if (!p) throw new Error(`Unknown Hex Layer parameter: ${id}`);
  edit(hexRef(id, p.count === 1 ? 0 : layer), raw);
}
window.hexEdit = hexEdit;

const hx = (id, layer = 0) => hexRef(id, layer);
const hctl = (id, layer, label = null, size = '') => knob(hx(id, layer), { label: label || hx(id, layer).name, size });
function hexWavePicker(layer) {
  const ref = hx('hexWaveNumber', layer);
  const button = el('button', { class: 'wave', title: 'Choose PCM wave' });
  const index = v => v == null ? null : v === 0 ? 0 : v - 326;
  watch(ref, v => { const i = index(v); button.textContent = i == null ? '—' : `${String(i).padStart(3, '0')}  ${hexData.waves[i] || 'Unknown wave'}`; });
  button.addEventListener('click', e => {
    e.stopPropagation();
    chooser(button, hexData.waves, index(vals.get(ref.key)), i => hexEdit('hexWaveNumber', layer, i + 326), { what: 'waves', cols: true, width: 700, wave: i => ['pcm', i, key()] });
  });
  // a layer plays the sample of the key its Coarse pitch moves middle C to
  const coarse = hx('hexPitchKey', layer), key = () => clamp(60 + (vals.get(coarse.key) ?? 64) - 64, 0, 127);     // the page holds the wire value, 64 = no shift
  const picture = wavePicture(() => (index(vals.get(ref.key)) == null ? null : ['pcm', index(vals.get(ref.key)), key()]));
  watch(ref, picture.draw);
  watch(coarse, picture.draw);
  return [button, picture.wrap];
}
function hexRangeBar(layer) {
  const bar = el('div', { class: 'hex-range', role: 'img', 'aria-label': 'Key range' });
  const active = el('i');
  bar.append(active, ...Array.from({ length: 12 }, (_, i) => el('b', { class: i % 2 ? 'black' : '' })));
  const low = hx('hexKeyRangeLow', layer), high = hx('hexKeyRangeHigh', layer);
  const update = () => {
    const a = vals.get(low.key), b = vals.get(high.key);
    if (a == null || b == null) { bar.classList.add('wait'); return; }
    bar.classList.remove('wait');
    active.style.left = `${100 * a / 127}%`;
    active.style.width = `${100 * Math.max(0, b - a) / 127}%`;
    bar.title = `${NOTES[a]} – ${NOTES[b]}`;
  };
  watch(low, update); watch(high, update);
  return bar;
}
function hexStrip(layer) {
  const strip = el('article', { class: 'hex-strip', 'data-layer': layer });
  const enabled = led(hx('hexOnoff', layer), `Layer ${layer + 1} on/off`);
  const title = el('b', { text: `LAYER ${layer + 1}` });
  const wave = hexWavePicker(layer);
  const volume = hctl('hexVolumeOfs', layer, 'Level', 'sm');
  const range = hexRangeBar(layer);
  strip.append(el('div', { class: 'hex-strip-head' }, title, enabled), ...wave, volume, range);
  strip.addEventListener('click', e => { if (!e.target.closest('button, .ctl')) showHexLayer(layer); });
  watch(hx('hexOnoff', layer), v => strip.classList.toggle('off', v === 0));
  return strip;
}
function showHexLayer(layer) {
  hexSelectedLayer = layer; window.hexSelectedLayer = layer;
  document.querySelectorAll('.hex-strip').forEach(strip => strip.classList.toggle('selected', +strip.dataset.layer === layer));
  const rank = r => r.ct !== 8 ? 0 : (r.id === 'hexOnoff' || r.id === 'hexWaveNumber' || r.id === 'hexVolumeOfs') ? 1
    : (r.count === 1 || r.inst === layer) ? 2 : 3 + r.inst;
  queue.sort((a, b) => rank(a) - rank(b));
  pump();
  const detail = $('#hexDetail');
  if (hexOpenLayer === layer && !detail.hidden) { detail.hidden = true; hexOpenLayer = null; return; }
  hexOpenLayer = layer;
  detail.hidden = false;
  detail.classList.add('hex-detail');
  detail.replaceChildren(
    el('h2', {}, el('span', { text: `Layer ${layer + 1}` }), el('em', { text: 'pitch, amp, filter, sends and ranges' }),
      el('button', { class: 'hex-close', text: '×', title: 'Close layer details', onclick: () => { detail.hidden = true; hexOpenLayer = null; } })),
    el('div', { class: 'hex-detail-grid' },
      hexPanel('Pitch', hctl('hexPitchKey', layer, 'Coarse'), hctl('hexPanOffset', layer, 'Pan')),
      hexGraphPanel('Amp envelope', 'ampl', offsetEnvelope({
          attack: { ref: hx('hexAmpAttackOfs', layer), centre: 128, span: 128, faster: 1 },
          decay: { ref: hx('hexAmpDecayOfs', layer), centre: 128, span: 128, faster: 1 },
          sustain: { ref: hx('hexAmpSustainOfs', layer), centre: 128, span: 128 },
          release: { ref: hx('hexAmpReleaseOfs', layer), centre: 128, span: 128, faster: 1 },
        }, 'drag the corners · dashed: the wave’s own'),
        hctl('hexAmpAttackOfs', layer, 'Attack'), hctl('hexAmpDecayOfs', layer, 'Decay'), hctl('hexAmpSustainOfs', layer, 'Sustain'), hctl('hexAmpReleaseOfs', layer, 'Release'), hctl('hexTouchSenseOfs', layer, 'Touch')),
      hexGraphPanel('Filter', 'filt', lowpassCurve([hx('hexCutoffOfs', layer)], () => {
          const v = vals.get(hx('hexCutoffOfs', layer).key);
          return v == null || v >= 126 ? null : 100 * 2 ** (v / 19);     // from the hardware's corner by offset, roughly
        }, 'low-pass · open at 0'),
        hctl('hexCutoffOfs', layer, 'Cutoff')),
      hexPanel('Sends', hctl('hexReverbSendOfs', layer, 'Reverb'), hctl('hexChorusSendOfs', layer, 'Chorus')),
      hexPanel('Keyboard range', hexRangeBar(layer), rangeControls(layer)),
      hexPanel('Velocity range', rangeControls(layer, true)),
      ...(layer === 1 || layer === 3 || layer === 5 ? [hexPanel('Pitch lock', led(hx('hexPitchLock', layer), 'Lock pitch to preceding layer'))] : [])
    ));
}
function hexGraphPanel(title, hue, picture, ...contents) {
  return el('section', { class: `hex-panel wide ${hue}` }, el('h3', { text: title }), picture, el('div', { class: 'ctls' }, ...contents));
}
const HEX_LFO_SHAPES = ['sine', 'triangle', 'sawUp', 'sawDown', 'pulse13', 'square', 'pulse31'];
function hexPanel(title, ...contents) { return el('section', { class: 'hex-panel' }, el('h3', { text: title }), el('div', { class: 'ctls' }, ...contents)); }
function rangeControls(layer, velocity = false) {
  const prefix = velocity ? 'hexVelRange' : 'hexKeyRange';
  return el('div', { class: 'hex-range-controls' },
    hctl(`${prefix}Low`, layer, 'Low'), hctl(`${prefix}High`, layer, 'High'));
}
function lfoBlock(title, start) {
  const key = type => `hex${start}${type}`;
  return el('section', { class: 'group hex-lfo lfo' },
    el('h2', { text: title }),
    lfoGraph({ wave: hx(key('LfoWave')), shapes: HEX_LFO_SHAPES, rate: hx(key('LfoRate')), delay: hx(key('AutoDelay')), rise: hx(key('AutoRise')),
               depth: hx(key('AutoDepth')), depthCentre: 128, depthSpan: 128 }, 'after a note: delay, rise, then steady'),
    el('div', { class: 'fields' }, pick(hx(key('LfoWave')), hexData.lfoWave, { label: 'Wave', number: false })),
    el('div', { class: 'ctls' },
      hctl(key('LfoRate'), 0, 'Rate', 'big'), hctl(key('AutoDelay'), 0, 'Delay'), hctl(key('AutoRise'), 0, 'Rise'),
      hctl(key('AutoDepth'), 0, 'Depth'), hctl(key('ModDepth'), 0, 'Wheel'), hctl(key('AfterDepth'), 0, 'After')));
}
function buildHexPage() {
  $('#hexStrips').replaceChildren(...Array.from({ length: 6 }, (_, i) => hexStrip(i)));
  $('#hexLfos').replaceChildren(lfoBlock('Pitch LFO', 'Pitch'), lfoBlock('Amp LFO', 'Amp'),
    el('section', { class: 'group hex-lfo detune' }, el('h2', { text: 'Performance' }), hctl('hexDetuneNumber', 0, 'Detune')));
  $('#hexPage').hidden = false;
  $('#hexPage').hidden = true;
}
function showHexPreset() {
  if (document.body.matches('.perform-mode, .panel-mode')) return;
  const n = hexProgram;
  $('#presetNum').textContent = n == null ? '---' : String(n).padStart(3, '0');
  $('#presetName').textContent = n == null ? 'Hex Layer preset' : hexData.presets[n];
}
// Show the Hex Layer page for preset n (null: a Hex tone that is not a factory preset).
function enterHex(n) {
  window.pcmToneTarget = null;
  hexProgram = n; window.hexProgram = n;
  activeEngine = 'hex'; window.activeEngine = 'hex';
  document.body.classList.add('hex-mode');
  document.body.classList.remove('draw-mode', 'pcm-mode');
  $('#soloPage').hidden = true; $('#hexPage').hidden = false;
  $('#drawPage').hidden = true; $('#pcmPage').hidden = true;
  $('#fx').hidden = true; $('#engineFx').hidden = false;
  $('#soloTab').classList.remove('on'); $('#hexTab').classList.toggle('on', !document.body.matches('.perform-mode, .panel-mode'));
  $('#drawTab').classList.remove('on'); $('#pcmTab').classList.remove('on');
  showHexPreset();
  window.updateVelocityButtons?.();
}
window.enterHex = enterHex;
function chooseHexPreset(n) {
  n = (n + hexData.presets.length) % hexData.presets.length;
  enterHex(n);
  releaseAll();
  discardToneEdits();
  sendMidi([0xB0, 0, 97, 0xB0, 0x20, 0, 0xC0, n]);
  for (const ref of refs.values()) if (ref.ct === 8 || ref.ct === 9) forget(ref);
  queue = []; inflight.clear();
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(() => readPatch(false), 450);
  showHexPreset();
}
window.chooseHexPreset = chooseHexPreset;
window.showHexPreset = showHexPreset;
window.initHex = async function initHex() {
  hexData = await (await fetch('hex.json')).json();
  window.hexData = hexData;
  for (const p of hexData.params) for (let i = 0; i < p.count; i++) {
    if (p.id === 'hexPitchLock' && i % 2 === 0) continue;
    hexRef(p.id, i);
  }
  buildHexPage();
  return hexData;
};
