// Normal DSP effects used by Hex Layer and Drawbar Organ tones.
'use strict';

let dspData;
let engineTypeRef;
const engineParamRefs = [];
let engineLineRef;
let lastEngineType;

function queueEngineParams() {
  for (const ref of engineParamRefs) {
    const f = inflight.get(ref.key);
    if (f) { f.ignore = true; f.refresh = true; }
    else { queue = queue.filter(r => r !== ref); queue.unshift(ref); total++; }
  }
  pump();
}

function buildEngineEffect() {
  const g = $('#engineFx'), body = el('div', { class: 'engine-fx-body' });
  const typeButton = el('button', { class: 'pick', title: 'DSP effect type' }, el('small', { text: 'Effect' }), el('b'));
  watch(engineTypeRef, value => { $('b', typeButton).textContent = value == null ? '–' : dspData.types.find(t => t.id === value)?.name || (value > 127 ? 'Not loaded' : `Type ${value}`); });
  typeButton.addEventListener('click', e => {
    e.stopPropagation();
    const current = dspData.types.findIndex(t => t.id === vals.get(engineTypeRef.key));
    chooser(typeButton, dspData.types.map(t => t.name), current, i => edit(engineTypeRef, dspData.types[i].id), {
      what: 'effects', cols: true, width: 480
    });
  });
  g.replaceChildren(el('h2', {}, el('span', { text: 'DSP' }), el('em', { text: 'normal tone effect' }),
    el('span', { class: 'right' }, field('DSP', led(engineLineRef, 'Line Select')))),
    el('div', { class: 'fields', style: 'margin-bottom:10px' }, typeButton), body);
  let shownType;
  watch(engineTypeRef, value => {
    if (value == null) { body.replaceChildren(); shownType = undefined; lastEngineType = undefined; return; }
    const type = dspData.types.find(t => t.id === value);
    if (!type) {
      // 129..134 are the Solo Synth's own effects: what the effect buffer still holds when a tone with its DSP
      // off was chosen after a Solo tone (the emulated instrument leaves it; the real one reports the tone's effect)
      shownType = undefined;
      body.replaceChildren(el('div', { class: 'note', text: value > 127
        ? 'This tone has its DSP off and no effect is loaded. Choose one above, then switch DSP on.'
        : `Effect type ${value} is not one this page knows.` }));
      return;
    }
    if (shownType !== type.id) {
      shownType = type.id;
      body.replaceChildren(type.params.length ? ctls(type.params.map((name, i) => {
        const ref = engineParamRefs[i];
        ref.name = `${type.name} ${name}`;
        return knob(ref, { label: name, size: i < 2 ? 'big' : '' });
      })) : el('div', { class: 'note', text: 'This effect has no parameters.' }));
    }
    if (lastEngineType !== undefined && lastEngineType !== value) queueEngineParams();
    lastEngineType = value;
  });
}

window.initFx = async function initFx() {
  dspData = await (await fetch('dsp.json')).json();
  engineTypeRef = makeRef(0x13, 2, 0, 0, { vt: 'u14', min: 0, max: 16383, name: 'Effect type', id: 'engineDspType' });
  for (let i = 0; i < 8; i++) engineParamRefs.push(makeRef(0x13, 3, 0, i, {
    vt: 'nf', min: 0, max: 127, name: `DSP Parameter ${i + 1}`, id: 'engineDspParam', count: 8
  }));
  engineLineRef = makeRef(2, 0x72, 0, 0, { vt: 'nf', min: 0, max: 1, name: 'Line Select', id: 'engineLineSelect' });
  buildEngineEffect();
  return dspData;
};
