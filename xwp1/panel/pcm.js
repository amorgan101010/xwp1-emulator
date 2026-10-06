// PCM tone editor. Values are shared SysEx refs and are always populated by readback.
'use strict';

let pcmData;
let pcmProgram = null;
const pcmRefs = {};
const pcmVals = {};
window.pcmData = null;
window.pcmVals = pcmVals;
window.pcmProgram = pcmProgram;
window.pcmToneTarget = null;

function pcmRef(id) {
  if (pcmRefs[id]) return pcmRefs[id];
  const p = pcmData.params.find(x => x.id === id);
  if (!p) throw new Error(`Unknown PCM parameter: ${id}`);
  const ref = makeRef(p.ct, p.pid, 0, 0, {
    vt: p.vt, min: p.min, max: p.max, name: p.name, id
  });
  pcmRefs[id] = ref;
  watch(ref, v => { if (v == null) delete pcmVals[id]; else pcmVals[id] = v; });
  return ref;
}

function pcmEdit(id, value) { edit(pcmRef(id), value); }
window.pcmEdit = pcmEdit;

const pctl = (id, label = null, size = '') => knob(pcmRef(id), { label: label || pcmRef(id).name, size });

function pcmSwitch(ref, labels, values) {
  const box = el('div', { class: 'seg', role: 'radiogroup', 'aria-label': ref.name });
  const buttons = labels.map((label, i) => {
    const b = el('button', { text: label, role: 'radio' });
    b.addEventListener('click', () => edit(ref, values[i]));
    box.append(b);
    return b;
  });
  watch(ref, v => buttons.forEach((b, i) => {
    const on = v === values[i];
    b.classList.toggle('on', on);
    b.setAttribute('aria-checked', on);
  }));
  return box;
}

function pcmGroup(title, hue, ...controls) {
  const pictures = controls.filter(c => c.classList && c.classList.contains('gwrap'));
  return el('section', { class: `group pcm-group ${hue}` },
    el('h2', { text: title }), ...pictures, el('div', { class: 'ctls' }, ...controls.filter(c => !pictures.includes(c))));
}

function buildPcmPage() {
  const title = el('section', { class: 'pcm-title group' },
    el('span', { class: 'pcm-number', text: '---' }),
    el('h1', { text: 'Reading PCM tone…' }));
  watch(toneNumber(), value => {
    const valid = value != null && value >= pcmData.first && value < pcmData.first + pcmData.tones.length;
    const index = valid ? value - pcmData.first : -1;
    pcmProgram = valid ? value : null;
    window.pcmProgram = pcmProgram;
    title.querySelector('.pcm-number').textContent = valid ? String(value).padStart(3, '0') : '---';
    title.querySelector('h1').textContent = valid ? pcmData.tones[index] : 'Reading PCM tone…';
  });
  const groups = el('div', { class: 'pcm-groups' },
    pcmGroup('Envelope', 'ampl', offsetEnvelope({
        attack: { ref: pcmRef('pcmAttackTime'), centre: 0, span: 64, faster: -1 },
        release: { ref: pcmRef('pcmReleaseTime'), centre: 0, span: 64, faster: -1 },
      }, 'drag the corners · dashed: the tone’s own'), pctl('pcmAttackTime', 'Attack'), pctl('pcmReleaseTime', 'Release'), pctl('pcmTouchSense', 'Touch')),
    pcmGroup('Filter', 'filt', lowpassCurve([pcmRef('pcmCutoffFreq')], () => {
        const v = vals.get(pcmRef('pcmCutoffFreq').key);
        return v == null || v >= 0 ? null : 12000 * 2 ** (v / 10);       // a picture of "darker", not a measured corner
      }, 'darker below 0'), pctl('pcmCutoffFreq', 'Cutoff')),
    pcmGroup('Vibrato', 'lfo', lfoGraph({ wave: pcmRef('pcmVibratoType'), shapes: ['sine', 'triangle', 'sawUp', 'square'],
        rate: pcmRef('pcmVibratoSpeed'), rateRange: [-64, 63], depth: pcmRef('pcmVibratoDepth'), depthCentre: 0, depthSpan: 64 },
      'vibrato'), pcmSwitch(pcmRef('pcmVibratoType'), ['Sine', 'Triangle', 'Saw', 'Square'], [0, 1, 2, 3]),
      pctl('pcmVibratoDepth', 'Depth'), pctl('pcmVibratoSpeed', 'Speed'), pctl('pcmVibratoDelay', 'Delay')),
    pcmGroup('Tone', 'pitch', pcmSwitch(pcmRef('pcmOctaveShift'), ['−2', '−1', '0', '+1', '+2'], [-2, -1, 0, 1, 2]),
      pctl('pcmVolume', 'Volume', 'big')));
  $('#pcmPage').replaceChildren(title, groups);
}

function enterPcm(number) {
  pcmProgram = number != null && number >= pcmData.first && number < pcmData.first + pcmData.tones.length ? number : null;
  window.pcmProgram = pcmProgram;
  if (!document.body.matches('.perform-mode, .panel-mode')) {
    $('#presetNum').textContent = pcmProgram == null ? '---' : String(pcmProgram).padStart(3, '0');
    $('#presetName').textContent = pcmProgram == null ? 'Reading PCM tone…' : pcmData.tones[pcmProgram - pcmData.first];
  }
  activeEngine = 'pcm'; window.activeEngine = 'pcm';
  document.body.classList.add('pcm-mode');
  document.body.classList.remove('hex-mode', 'draw-mode');
  $('#soloPage').hidden = true; $('#hexPage').hidden = true; $('#drawPage').hidden = true; $('#pcmPage').hidden = false;
  $('#fx').hidden = true; $('#engineFx').hidden = false;
  $('#soloTab').classList.remove('on'); $('#hexTab').classList.remove('on'); $('#drawTab').classList.remove('on');
  $('#pcmTab').classList.toggle('on', !document.body.matches('.perform-mode, .panel-mode'));
  window.updateVelocityButtons?.();
}
window.enterPcm = enterPcm;

function choosePcmTone(number) {
  const n = Math.max(pcmData.first, Math.min(pcmData.first + pcmData.tones.length - 1, Math.round(number)));
  window.pcmToneTarget = n;
  releaseAll();
  discardToneEdits();
  // Unlike a bank/program change, PCM tones are selected by writing toneNumber directly.
  sendMidi(setMessage(toneNumber(), n));
  for (const ref of refs.values()) if (ref !== toneNumber()) forget(ref);
  queue = []; inflight.clear();
  enterPcm(null);
  store(toneNumber(), n);
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(() => readPatch(false), 450);
}
window.choosePcmTone = choosePcmTone;

window.initPcm = async function initPcm() {
  pcmData = await (await fetch('pcm.json')).json();
  window.pcmData = pcmData;
  for (const p of pcmData.params) pcmRef(p.id);
  buildPcmPage();
  return pcmData;
};
