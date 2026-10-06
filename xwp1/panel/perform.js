// The instrument's own front panel: display, buttons, LEDs and the 16 step
// buttons, for what the firmware does by itself (Performances, step
// sequencer, chains, phrases, arpeggio). The engine answers F0 7D 58 4C with
// the LEDs, the display memory and the positions of the sliders and knobs, and
// takes buttons as F0 7D 58 42 code down, the dial as 44 clicks, a slider or
// knob as 41 control position (xwp1/src/front.rs). Button code = matrix column * 8 + row.
'use strict';

(() => {
  const FRONT = [0xF0, 0x7D, 0x58, 0x4C], BUTTON = [0xF0, 0x7D, 0x58, 0x42], DIAL = [0xF0, 0x7D, 0x58, 0x44], CONTROL = [0xF0, 0x7D, 0x58, 0x41];
  // name, code, LED (null: none), hint
  const b = (name, code, led = null, hint = '', cls = '', secondary = '') => ({ name, code, led, hint, cls, secondary });
  const GROUPS = [
    { id: 'mode', title: 'Mode', keys: [
      b('Perform', 0x0E, 37, 'Performance mode'), b('Tone', 0x15, 6, 'Tone mode'), b('Step Seq', 0x04, 5, 'Step sequencer mode'),
      b('Mixer', 0x29, null, 'Mixer'), b('Edit', 0x06, null, 'Edit what the mode shows'), b('Write', 0x0C, null, 'Store to user memory'),
      b('Setting', 0x0D, null, 'Instrument settings'), b('Menu', 0x27, null, 'Mode menu') ] },
    { id: 'tones', title: 'Tone / Pattern', keys: [
      b('Solo', 0x19, 65, 'Solo Synth tones; pattern 1', 'n1'), b('Hex Layer', 0x17, 38, 'Hex Layer tones; pattern 2', 'n2'),
      b('Organ', 0x0F, 70, 'Drawbar Organ tones; pattern 3', 'n3'), b('Piano', 0x07, 69, 'PCM piano tones; pattern 4', 'n4'),
      b('Str / Brs', 0x0B, 68, 'PCM strings and brass; pattern 5', 'n5'), b('Gt / Bass', 0x03, 67, 'PCM guitar and bass; pattern 6', 'n6'),
      b('Synth', 0x12, 66, 'PCM synth tones; pattern 7', 'n7'), b('Various', 0x18, 64, 'PCM various tones; pattern 8', 'n8') ] },
    { id: 'seq', title: 'Step sequencer', keys: [
      b('Start / Stop', 0x09, 2, 'Start or stop the step sequencer', 'wide go'), b('Chain', 0x00, 3, 'Chain mode'),
      b('Part −', 0x11), b('Part +', 0x02), b('Step −', 0x08, null, 'Step back; Delete'), b('Step +', 0x01, null, 'Step forward; Insert') ] },
    { id: 'tempo', title: 'Tempo', keys: [ b('−', 0x05, null, 'Tempo down'), b('+', 0x14, null, 'Tempo up'), b('Tap', 0x16, 4, 'Tap the tempo') ] },
    { id: 'phrase', title: 'Phrase', keys: [
      b('Rec', 0x0A, 33, 'Record a phrase', 'red'), b('Play / Stop', 0x21, 35, 'Play or stop the phrase', 'go'), b('Key Play', 0x13, 1, 'Start the phrase from the keyboard') ] },
    { id: 'arp', title: 'Arpeggio', keys: [ b('Arpeggio', 0x28, 34, 'Arpeggio on / off (hold: choose)'), b('Hold', 0x30, 40, 'Hold') ] },
    { id: 'shift', title: 'Keyboard', keys: [ b('Oct −', 0x2E, null, 'Octave down'), b('Oct +', 0x23, null, 'Octave up'), b('Transpose', 0x1F) ] },
    { id: 'pad', title: 'Number', keys: [
      b('7', 0x2D), b('8', 0x2C), b('9', 0x24), b('4', 0x32), b('5', 0x26), b('6', 0x25), b('1', 0x35), b('2', 0x34), b('3', 0x2B),
      b('No −', 0x1E, null, 'No; one down'), b('0', 0x36), b('Yes +', 0x33, null, 'Yes; one up'),
      b('Num / Bank', 0x2F, 32, 'Number keys choose the number or the bank', 'wide'), b('Preset / User', 0x37, null, '', 'wide') ] },
    { id: 'cursor', title: 'Cursor', keys: [
      b('▲', 0x1D, null, 'Up', 'up'), b('◀', 0x1B, null, 'Left', 'left'), b('Enter', 0x1A, null, '', 'enter'), b('▶', 0x2A, null, 'Right', 'right'),
      b('▼', 0x22, null, 'Down', 'dir-down'), b('Exit', 0x1C, null, '', 'exit') ] },
    { id: 'sliders', title: 'Sliders play', keys: [
      b('Solo Synth', 0x38, 78), b('Hex Layer', 0x3B, 77), b('Organ', 0x3A, 76, 'Drawbar Organ'), b('Step Seq', 0x39, 75) ] },
    { id: 'organ', title: 'Organ', keys: [
      b('Perc 2nd', 0x3D, 39, 'Organ: second-harmonic percussion. Sequencer / mixer: Func A/B', '', 'Func A/B'),
      b('Perc 3rd', 0x3C, 71, 'Organ: third-harmonic percussion. Sequencer / mixer: 1–8 / 9–16', '', '1–8 / 9–16'),
      b('Rotary', 0x3E, 7, 'Organ: rotary slow / fast. Sequencer: Key Shift; mixer: master / external input', 'wide', 'Key Shift') ] },
  ];
  const STEP_LED = [8, 9, 10, 11, 12, 13, 14, 72, 41, 42, 43, 44, 45, 46, 73, 74];
  const stepCode = n => n < 8 ? 0x4F - n : 0x47 - (n - 8);

  const COLS = 72, ROWS = 16, PITCH = 6;      // CSS pixels per dot
  const STRIP = 5;                            // rows of dots given to the digits under the dot matrix
  const ram = new Uint8Array(362);
  let leds = 0n, arpHold = null, page = null, panelPage = null, editor = null, mixer = null, grid = null, canvas = null, lamps = [], shown = false, view = 'perform', got = false;
  let which = 'edit';       // the Performance editor tab last shown
  try { which = ['edit', 'seq', 'mix'].find(t => t === localStorage.getItem('xwp1:perform')) || 'edit'; } catch (e) { /* a private window */ }
  let keysWas = null;       // the player's key mode before this view switched it on

  let touched = false;      // something was pressed or turned here: the tone the editor pages show may no longer be the part's
  let holdEditsPerformance = false;
  const inPerformMode = () => (leds >> 37n & 1n) === 1n;
  const sendButton = (code, down) => {
    if (code === 0x30) {
      if (down) holdEditsPerformance = inPerformMode();
      if (holdEditsPerformance) {
        if (!down) { holdEditsPerformance = false; window.performanceEditor.toggleHold(); }
        return;
      }
    }
    touched = true; sendMidi([...BUTTON, code, down ? 1 : 0, 0xF7]);
  };
  function press(code, ms = 60) { sendButton(code, true); return new Promise(r => setTimeout(() => { sendButton(code, false); r(); }, ms)); }

  function button(k) {
    const node = el('button', { class: `fp-btn ${k.cls}`, title: k.hint || k.name, 'data-code': k.code },
      k.led != null ? el('i', { class: 'lamp' }) : null, el('span', { text: k.name }),
      k.secondary ? el('small', { text: k.secondary }) : null);
    if (k.led != null) lamps.push([k.led, node]);
    let down = false;
    const set = on => { if (on !== down) { down = on; node.classList.toggle('down', on); sendButton(k.code, on); } };
    node.addEventListener('pointerdown', e => { e.preventDefault(); node.setPointerCapture(e.pointerId); set(true); });
    for (const type of ['pointerup', 'pointercancel', 'lostpointercapture']) node.addEventListener(type, () => set(false));
    node.addEventListener('keydown', e => { if ((e.key === ' ' || e.key === 'Enter') && !e.repeat) set(true); });
    node.addEventListener('keyup', e => { if (e.key === ' ' || e.key === 'Enter') set(false); });
    node.addEventListener('blur', () => set(false));
    return node;
  }

  // Sliders 1..8, MASTER and knobs 1..4: positions 0..127 the engine turns into converter readings.
  // They are not firmware parameters: made-up refs, as the macros have, so the knob control can draw them.
  const cref = n => ({ key: 'front:control' + n, min: 0, max: 127, vt: 'nf', name: n < 8 ? `Slider ${n + 1}` : n === 8 ? 'Master slider' : `Knob ${n - 8}` });
  function setControl(n, v) {
    v = clamp(Math.round(v), 0, 127);
    if (vals.get(cref(n).key) === v) return;
    store(cref(n), v); touched = true;
    sendMidi([...CONTROL, n, v, 0xF7]);
  }
  function fader(n, label) {
    const ref = cref(n), fill = el('b'), thumb = el('i'), out = el('output');
    const track = el('div', { class: 'fp-track', tabindex: 0, role: 'slider', 'aria-label': ref.name, 'aria-valuemin': 0, 'aria-valuemax': 127 }, fill, thumb);
    const box = el('div', { class: 'fp-fader' + (n === 8 ? ' master' : ''), title: ref.name }, track, out, el('label', { text: label }));
    watch(ref, v => {
      const f = (v ?? 0) / 127;
      fill.style.height = `${f * 100}%`; thumb.style.bottom = `calc(${f} * (100% - 12px))`;
      out.textContent = v ?? 0; track.setAttribute('aria-valuenow', v ?? 0);
    });
    const at = e => { const r = track.getBoundingClientRect(); setControl(n, 127 * (r.bottom - 6 - e.clientY) / (r.height - 12)); };
    let drag = false;
    track.addEventListener('pointerdown', e => { e.preventDefault(); track.setPointerCapture(e.pointerId); drag = true; at(e); });
    track.addEventListener('pointermove', e => { if (drag) at(e); });
    for (const type of ['pointerup', 'pointercancel']) track.addEventListener(type, () => { drag = false; });
    const nudge = d => setControl(n, (vals.get(ref.key) ?? 0) + d);
    track.addEventListener('wheel', e => { e.preventDefault(); nudge((e.deltaY < 0 ? 1 : -1) * (e.shiftKey ? 8 : 1)); }, { passive: false });
    track.addEventListener('keydown', e => {
      const d = { ArrowUp: 1, ArrowRight: 1, ArrowDown: -1, ArrowLeft: -1, PageUp: 10, PageDown: -10 }[e.key];
      if (d) { e.preventDefault(); nudge(d); }
    });
    return box;
  }
  // The data dial: no position, only clicks one way or the other.
  function dial() {
    const mark = el('i'), node = el('div', { class: 'fp-dial', tabindex: 0, role: 'spinbutton', 'aria-label': 'Dial', title: 'Dial: drag up or down, or use the mouse wheel' }, mark);
    let turned = 0, from = null;
    const turn = clicks => {
      if (!clicks) return;
      turned += clicks; touched = true;
      mark.style.transform = `rotate(${turned * 15}deg)`;
      sendMidi([...DIAL, clicks & 0x7F, 0xF7]);
    };
    node.addEventListener('pointerdown', e => { e.preventDefault(); node.setPointerCapture(e.pointerId); from = e.clientY; });
    node.addEventListener('pointermove', e => {
      if (from == null) return;
      const clicks = Math.trunc((from - e.clientY) / 9);
      if (clicks) { from -= clicks * 9; turn(clamp(clicks, -32, 32)); }
    });
    for (const type of ['pointerup', 'pointercancel']) node.addEventListener(type, () => { from = null; });
    node.addEventListener('wheel', e => { e.preventDefault(); turn(e.deltaY < 0 ? 1 : -1); }, { passive: false });
    node.addEventListener('keydown', e => {
      const d = { ArrowUp: 1, ArrowRight: 1, ArrowDown: -1, ArrowLeft: -1, PageUp: 10, PageDown: -10 }[e.key];
      if (d) { e.preventDefault(); turn(d); }
    });
    return el('div', { class: 'fp-dialbox' }, node, el('label', { text: 'Dial' }));
  }

  function build() {
    canvas = el('canvas', { class: 'fp-lcd', 'aria-label': 'Display' });
    const stepRow = from => el('div', { class: 'fp-steps' },
      Array.from({ length: 8 }, (_, k) => button(b(String(from + k + 1), stepCode(from + k), STEP_LED[from + k], `Step ${from + k + 1}`, 'step' + (k % 4 === 0 ? ' beat' : '')))));
    const groups = Object.fromEntries(GROUPS.map(g => [g.id,
      el('div', { class: `fp-group fp-${g.id}` }, el('h3', { text: g.title }), el('div', { class: 'fp-keys' }, g.keys.map(button)))]));
    groups.cursor.append(dial());
    groups.seq.append(stepRow(0), stepRow(8));
    for (let n = 0; n < 13; n++) if (vals.get(cref(n).key) == null) store(cref(n), 0);
    groups.organ.querySelector('h3').title = 'Secondary button labels apply to the sequencer / mixer';
    const controls = el('div', { class: 'fp-group fp-controls' },
      el('div', { class: 'fp-control-buttons' }, groups.sliders, groups.organ),
      el('div', { class: 'fp-faders' }, Array.from({ length: 9 }, (_, n) => fader(n, n === 8 ? 'Master' : `${n + 1}/${n + 9}`))),
      el('div', { class: 'fp-knobs' }, Array.from({ length: 4 }, (_, k) =>
        knob(cref(9 + k), { label: `Knob ${k + 1}`, set: v => setControl(9 + k, v), text: v => String(v ?? 0), reset: 64 }))));
    // the same buttons, with their LEDs, where the Performance editor wants them
    const named = { tap: [0x16, 4, 'Tap'], start: [0x09, 2, 'Start / Stop', 'go'], chain: [0x00, 3, 'Chain'], arp: [0x28, 34, 'Arpeggio'],
                    rec: [0x0A, 33, 'Rec', 'red'], play: [0x21, 35, 'Play / Stop', 'go'] };
    const buttonOf = id => { const [code, lamp, name, cls = ''] = named[id]; return button(b(name, code, lamp, '', cls)); };
    editor = window.performanceEditor.build(buttonOf);
    watch(window.performanceEditor.refs.arpHold, v => { arpHold = v; light(); });
    mixer = window.performanceEditor.buildMixer();
    grid = window.stepGrid.build(buttonOf, window.performanceEditor.seqChooser, window.performanceEditor.patternSeg);
    const tabs = el('div', { class: 'fp-tabs' },
      ...[['edit', 'Performance', 'Zones, tempo, step sequence, arpeggio and phrase by name'], ['seq', 'Sequence', 'The step sequence as a grid'], ['mix', 'Mixer', 'The 16 parts: tone, level, pan and sends']]
        .map(([id, text, title]) => { const t = el('button', { class: 'pill', 'data-tab': id, text, title }); t.addEventListener('click', () => tab(id)); return t; }));
    tabs.append(el('em', { id: 'fpNote' }));
    page = el('div', { id: 'performPage', hidden: '' }, tabs, editor, grid, mixer);
    panelPage = el('div', { id: 'frontPanelPage', hidden: '' },
      el('section', { class: 'group fp' },
        el('div', { class: 'fp-top' },
          el('div', { class: 'fp-left' }, groups.mode, groups.tones),
          el('div', { class: 'fp-screen' }, canvas, el('div', { class: 'fp-row' }, groups.tempo, groups.arp, groups.phrase, groups.shift)),
          el('div', { class: 'fp-right' }, groups.pad, groups.cursor)),
        el('div', { class: 'fp-mid' }, groups.seq, controls)));
    $('#app').prepend(page, panelPage);
    draw();
  }

  function draw() {
    if (!canvas) return;
    // a whole number of device pixels per dot, whatever the zoom, so the dots stay even
    const pitch = Math.max(2, Math.round(PITCH * (window.devicePixelRatio || 1))), gap = pitch > 3 ? 1 : 0;
    if (canvas.width !== COLS * pitch) { canvas.width = COLS * pitch; canvas.height = (ROWS + STRIP) * pitch; }
    const g = canvas.getContext('2d'), css = getComputedStyle(canvas);
    const [paper, ink, ghost] = ['--lcd', '--lcd-ink', '--lcd-ghost'].map(n => css.getPropertyValue(n).trim());
    g.fillStyle = paper; g.fillRect(0, 0, canvas.width, canvas.height);
    for (let r = 0; r < ROWS; r++) {
      for (let c = 0; c < COLS; c++) {
        g.fillStyle = ram[r * 9 + (c >> 3)] >> (7 - (c & 7)) & 1 ? ink : ghost;
        g.fillRect(c * pitch, r * pitch, pitch - gap, pitch - gap);
      }
    }
    // the segment half, as far as it is known: tempo, measure and beat
    const top = Math.round((ROWS + 0.8) * pitch), h = Math.round((STRIP - 1.3) * pitch), w = Math.round(h * 0.56), t = Math.max(2, Math.round(pitch / 3));
    g.font = `600 ${Math.round(pitch * 1.25)}px Chakra, sans-serif`; g.textBaseline = 'middle';
    let x = pitch;
    for (const [name, digits] of READOUT) {
      g.fillStyle = ink; g.fillText(name, x, top + h / 2); x += g.measureText(name).width + pitch;
      for (const cell of digits) {
        const on = segments(cell), half = (h - 3 * t) / 2;
        const bar = (lit, bx, by, bw, bh) => { g.fillStyle = lit ? ink : ghost; g.fillRect(x + bx, top + by, bw, bh); };
        bar(on[0], t, 0, w - 2 * t, t); bar(on[6], t, t + half, w - 2 * t, t); bar(on[3], t, h - t, w - 2 * t, t);
        bar(on[5], 0, t, t, half); bar(on[1], w - t, t, t, half); bar(on[4], 0, 2 * t + half, t, half); bar(on[2], w - t, 2 * t + half, t, half);
        x += w + Math.round(pitch * 0.7);
      }
      x += pitch * 3;
    }
  }
  // Seven-segment cells in the display memory: four rows of 24 bits from byte 345, a digit on two columns. The
  // left column carries f, g, e on rows 1..3, or on rows 0..2 for the tens of tempo and measure (FINDINGS).
  const bitAt = (row, col) => ram[345 + 3 * row + (col >> 3)] >> (7 - (col & 7)) & 1;
  const segments = ([col, early]) => { const o = early ? 0 : 1; return [bitAt(0, col + 1), bitAt(1, col + 1), bitAt(2, col + 1), bitAt(3, col + 1), bitAt(o + 2, col), bitAt(o, col), bitAt(o + 1, col)]; };
  const READOUT = [['TEMPO', [[2, false], [4, true], [6, false]]], ['MEASURE', [[8, false], [10, true], [12, false]]], ['BEAT', [[14, false]]]];
  const GLYPH = { 1111110: 0, 110000: 1, 1101101: 2, 1111001: 3, 110011: 4, 1011011: 5, 1011111: 6, 1110000: 7, 1111111: 8, 1111011: 9, 0: '' };
  const number = digits => { const text = digits.map(cell => GLYPH[Number(segments(cell).join(''))] ?? '?').join(''); return /^\d+$/.test(text) ? Number(text) : null; };
  const readout = () => Object.fromEntries(READOUT.map(([name, digits]) => [name.toLowerCase(), number(digits)]));
  function light() {
    for (const [n, node] of lamps) node.classList.toggle('lit', n === 40 && inPerformMode() ? arpHold === 1 : (leds >> BigInt(n) & 1n) === 1n);
  }

  // answers from the engine (app.js passes them on)
  window.frontPanel = msg => {
    got = true;
    if (msg[4] === 0) {
      leds = 0n;
      for (let i = 0; i < 12; i++) leds |= BigInt(msg[5 + i]) << BigInt(7 * i);
      light();
    } else if (msg[4] === 1) {
      for (let i = 0; i < ram.length; i++) ram[i] = msg[5 + 2 * i] << 4 | msg[6 + 2 * i];
      draw();
    }
    else if (msg[4] === 2) for (let n = 0; n < 13; n++) store(cref(n), msg[5 + n]);
  };
  // "K 0|1": whether notes play the instrument's own keyboard. This view needs it (zones, arpeggio, phrase recording).
  window.playerKeys = text => {
    const on = text[2] !== '0';
    if (!shown) return;
    if (keysWas == null) keysWas = on;
    if (!on) ws.send('k1');
  };
  const ask = () => { if (linked) { sendMidi([...FRONT, 0xF7]); if (shown) { ws.send('k?'); if (page) window.performanceEditor.show(true, view === 'panel' ? 'panel' : which); } } };
  window.frontLinked = ask;

  function tab(id) {
    if (!['edit', 'seq', 'mix'].includes(id)) return;
    which = id;
    try { localStorage.setItem('xwp1:perform', id); } catch (e) { /* not kept */ }
    if (!page) return;
    editor.hidden = id !== 'edit'; mixer.hidden = id !== 'mix'; grid.hidden = id !== 'seq';
    for (const t of page.querySelectorAll('.fp-tabs .pill')) t.classList.toggle('on', t.dataset.tab === id);
    window.performanceEditor.show(shown && view === 'perform', id);
    window.stepGrid.show(shown && view === 'perform' && id === 'seq');
  }
  const TABS = ['soloTab', 'hexTab', 'drawTab', 'pcmTab'];
  function show(on, target = 'perform') {
    if (on === shown && (!on || view === target)) return;
    const switching = on && shown;
    if (switching) { window.performanceEditor.show(false); window.stepGrid.show(false); }
    if (on) view = target;
    shown = on;
    if (on && !page) build();
    document.body.classList.toggle('perform-mode', on && view === 'perform');
    document.body.classList.toggle('panel-mode', on && view === 'panel');
    page.hidden = !on || view !== 'perform';
    panelPage.hidden = !on || view !== 'panel';
    $('#performTab').classList.toggle('on', on && view === 'perform');
    $('#panelTab').classList.toggle('on', on && view === 'panel');
    if (on) {
      for (const id of TABS) { const t = $('#' + id); t.dataset.was = t.classList.contains('on') ? '1' : ''; t.classList.remove('on'); }
      if (view === 'perform') window.performanceEditor.paintPreset();
      releaseAll();
      if (!switching) { keysWas = null; touched = false; }
      ask();
      if (view === 'perform') {
        tab(which);
        $('#fpNote').textContent = poly.mode !== 'multi' && poly.voices > 1
          ? 'Several voices: zone 1 plays polyphonically; the sequencer and the other zones play the first voice.' : '';
      } else draw();
    } else {
      for (const id of TABS) $('#' + id).classList.toggle('on', id === ({ solo: 'soloTab', hex: 'hexTab', draw: 'drawTab', pcm: 'pcmTab' })[activeEngine]);
      showProgram();
      if (activeEngine === 'hex') window.showHexPreset();
      if (activeEngine === 'draw') window.showDrawPreset();
      releaseAll();
      window.performanceEditor.show(false);
      window.stepGrid.show(false);
      if (keysWas === false && linked) ws.send('k0');
      if (touched) {
        // a Performance or a tone button may have changed the part's tone, the sliders its values: read it all again
        for (const ref of refs.values()) forget(ref);
        clearTimeout(reloadTimer); reloadTimer = setTimeout(() => readPatch(true), 300);
      }
    }
  }
  $('#performTab').addEventListener('click', () => show(true, 'perform'));
  $('#panelTab').addEventListener('click', () => show(true, 'panel'));
  for (const id of TABS) $('#' + id).addEventListener('click', () => show(false), true);
  window.addEventListener('pagehide', () => { if (shown && keysWas === false && linked) ws.send('k0'); });

  window.front = { show, showPanel: on => show(on, 'panel'), press, ram, tab, readout, touch: () => { touched = true; }, control: setControl, dial: clicks => { touched = true; sendMidi([...DIAL, clicks & 0x7F, 0xF7]); }, leds: () => leds, got: () => got, text: () =>
    Array.from({ length: ROWS }, (_, r) => Array.from({ length: COLS }, (_, c) => ram[r * 9 + (c >> 3)] >> (7 - (c & 7)) & 1 ? '#' : '.').join('')) };
})();
