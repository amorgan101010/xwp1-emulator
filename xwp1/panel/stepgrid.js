// The step sequence being edited, as a grid: nine parts by sixteen steps of the
// pattern that plays. The steps have no parameter addresses and no bulk dump,
// so this reads and writes them where the firmware keeps them, in the
// instrument's work RAM (docs/FINDINGS.md, "Step data"): from 0x1c00596c,
// per part 8 patterns of 32 bytes (16 velocities, bit 7 = step off; 16 notes,
// bit 7 = tied to the step before), in the order Bass, Solo 1, Solo 2, Drum
// 1..5; then the Chord part, 64 bytes a pattern (velocities and three rows of
// notes). The engine reads with F0 7D 58 50 and writes with F0 7D 58 57.
'use strict';

(() => {
  const BASE = 0x1C00596C, SIZE = 0xA00;
  // as the instrument orders them; `at`: offset of the part's first pattern, `rows`: note rows
  const PARTS = [['Drum 1', 0x300], ['Drum 2', 0x400], ['Drum 3', 0x500], ['Drum 4', 0x600], ['Drum 5', 0x700],
                 ['Bass', 0x000], ['Solo 1', 0x100], ['Solo 2', 0x200], ['Chord', 0x800, 3]].map(([name, at, rows = 1]) => ({ name, at, rows }));
  const mem = new Uint8Array(SIZE);
  let have = false, shown = false, selected = 5, pattern = 0, page = null, cells = [], lane = null, timer = 0;
  const seven = (v, n) => Array.from({ length: n }, (_, i) => (v >> (7 * i)) & 127);
  const NOTE = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
  const noteName = n => NOTE[n % 12] + (Math.floor(n / 12) - 1);

  const velAt = (p, step) => PARTS[p].at + pattern * (PARTS[p].rows === 3 ? 64 : 32) + step;
  const noteAt = (p, step, row = 0) => velAt(p, 0) + 16 * (row + 1) + step;
  function read() {
    if (!linked) return;
    sendMidi([0xF0, 0x7D, 0x58, 0x50, ...seven(BASE, 5), ...seven(SIZE, 2), 0xF7]);
  }
  function write(offset, bytes) {
    bytes.forEach((b, i) => { mem[offset + i] = b; });
    sendMidi([0xF0, 0x7D, 0x58, 0x57, ...seven(BASE + offset, 5), ...bytes.flatMap(b => [b >> 4, b & 15]), 0xF7]);
    window.front.touch();
    paint();
  }
  window.synthTaps.push(msg => {
    if (msg.length !== 10 + 2 * SIZE || msg[0] !== 0xF0 || msg[1] !== 0x7D || msg[2] !== 0x58 || msg[3] !== 0x50 || msg[msg.length - 1] !== 0xF7) return false;
    if (msg.slice(4, 9).reduceRight((a, b) => a * 128 + b, 0) !== BASE) return false;      // the tone editor's own reads
    for (let i = 9; i < msg.length - 1; i++) if (msg[i] > 15) return false;
    for (let i = 0; i < SIZE; i++) mem[i] = msg[9 + 2 * i] << 4 | msg[10 + 2 * i];
    have = true;
    paint();
    return true;
  });
  const later = ms => { clearTimeout(timer); timer = setTimeout(read, ms); };

  // ---- drawing
  function paint() {
    if (!page) return;
    page.classList.toggle('wait', !have);
    cells.forEach((row, p) => row.forEach((cell, step) => {
      const v = mem[velAt(p, step)], tie = mem[noteAt(p, step)] & 0x80, on = !(v & 0x80);
      cell.classList.toggle('on', on); cell.classList.toggle('tie', !on && !!tie);
      cell.style.setProperty('--v', (v & 127) / 127);
      cell.title = `${PARTS[p].name}, step ${step + 1}: ${on ? `velocity ${v & 127}` : tie ? 'tied to the step before' : 'off'}`;
    }));
    for (const [p, node] of page.querySelectorAll('button.sg-part').entries()) node.classList.toggle('sel', p === selected);
    if (lane) lane();
  }
  function cell(p, step) {
    const node = el('button', { class: 'sg-cell' + (step % 4 === 0 ? ' beat' : '') }, el('i'));
    let start = null;
    node.addEventListener('pointerdown', e => {
      if (!have) return;
      e.preventDefault();
      try { node.setPointerCapture(e.pointerId); } catch (err) { /* a made-up pointer (tests) */ }
      selected = p;
      start = { y: e.clientY, v: mem[velAt(p, step)], moved: false, tie: e.shiftKey || e.button === 2 };
      paint();
    });
    node.addEventListener('pointermove', e => {
      if (!start || start.tie || Math.abs(start.y - e.clientY) < 4) return;
      start.moved = true;                 // a drag sets the velocity, and the step on
      const v = clamp(Math.round((start.v & 127) + (start.y - e.clientY) * 0.8), 1, 127);
      if (mem[velAt(p, step)] !== v) write(velAt(p, step), [v]);
      const r = node.getBoundingClientRect(); showTip(r.left + r.width / 2, r.top, String(v));
    });
    node.addEventListener('pointerup', () => {
      if (!start) return;
      hideTip();
      if (start.tie) {
        // tied: the note before sounds on through this step, which is itself off
        const n = mem[noteAt(p, step)];
        write(noteAt(p, step), [n ^ 0x80]);
        if (!(n & 0x80)) write(velAt(p, step), [mem[velAt(p, step)] | 0x80]);
      } else if (!start.moved) {
        const v = mem[velAt(p, step)];
        if (v & 0x80) write(noteAt(p, step), [mem[noteAt(p, step)] & 0x7F]);     // on: no longer a tie
        write(velAt(p, step), [v ^ 0x80]);
      }
      start = null;
    });
    node.addEventListener('pointercancel', () => { start = null; hideTip(); });
    node.addEventListener('contextmenu', e => e.preventDefault());
    return node;
  }
  // the selected part's notes, one box a step: drag or the wheel changes the note
  function buildLane(box) {
    const rows = [0, 1, 2].map(row => Array.from({ length: 16 }, (_, step) => {
      const node = el('button', { class: 'sg-note' + (step % 4 === 0 ? ' beat' : '') });
      const set = n => { const at = noteAt(selected, step, row); write(at, [mem[at] & 0x80 | clamp(n, 0, 127)]); };
      let start = null;
      node.addEventListener('pointerdown', e => { if (!have) return; e.preventDefault(); node.setPointerCapture(e.pointerId); start = { y: e.clientY, n: mem[noteAt(selected, step, row)] & 127 }; });
      node.addEventListener('pointermove', e => { if (start) set(Math.round(start.n + (start.y - e.clientY) / 5)); });
      for (const type of ['pointerup', 'pointercancel']) node.addEventListener(type, () => { start = null; });
      node.addEventListener('wheel', e => { e.preventDefault(); if (have) set((mem[noteAt(selected, step, row)] & 127) + (e.deltaY < 0 ? 1 : -1) * (e.shiftKey ? 12 : 1)); }, { passive: false });
      return node;
    }));
    const lines = rows.map((row, i) => el('div', { class: 'sg-row' }, el('span', { class: 'sg-part plain', text: i ? `Note ${i + 1}` : 'Note' }), el('div', { class: 'sg-steps' }, row)));
    const title = el('h3');
    box.append(title, ...lines);
    return () => {
      const part = PARTS[selected];
      title.textContent = `${part.name}: notes`;
      lines.forEach((line, i) => { line.hidden = i >= part.rows; });
      rows.forEach((row, i) => row.forEach((node, step) => {
        if (i >= part.rows) return;
        const n = mem[noteAt(selected, step, i)], off = mem[velAt(selected, step)] & 0x80;
        node.textContent = have ? noteName(n & 127) : '';
        node.classList.toggle('dim', !!off);
        node.title = `Step ${step + 1}: note ${n & 127}`;
      }));
    };
  }

  function build(buttonOf, chooserOf, patternSeg) {
    const grid = el('div', { class: 'sg-grid' });
    cells = PARTS.map((part, p) => {
      const row = Array.from({ length: 16 }, (_, step) => cell(p, step));
      const name = el('button', { class: 'sg-part', text: part.name });
      name.addEventListener('click', () => { selected = p; paint(); });
      grid.append(el('div', { class: 'sg-row' + (p === 5 || p === 8 ? ' gap' : '') }, name, el('div', { class: 'sg-steps' }, row)));
      return row;
    });
    const numbers = el('div', { class: 'sg-row head' }, el('span', { class: 'sg-part plain' }),
      el('div', { class: 'sg-steps' }, Array.from({ length: 16 }, (_, i) => el('span', { class: i % 4 === 0 ? 'beat' : '', text: i + 1 }))));
    const notes = el('div', { class: 'sg-lane' });
    lane = buildLane(notes);
    page = el('section', { class: 'group pe sg wait' },
      el('h2', {}, el('span', { text: 'Step sequence' }),
        el('em', { text: 'click a step on or off, drag up or down for its velocity, shift-click to tie it to the step before' })),
      el('div', { class: 'sg-head' }, chooserOf(), buttonOf('start'), field('Pattern', patternSeg()),
        el('span', { class: 'sg-hint', text: 'To keep it: Step Seq, then Write, on the Front panel tab.' })),
      numbers, grid, notes);
    paint();
    return page;
  }

  window.stepGrid = {
    build,
    show(on) { shown = on; clearTimeout(timer); if (on) read(); },
    // the pattern that plays (0..7), from the Performance editor's value; a new sequence or pattern is read again
    pattern(n) { if (n == null || n === pattern) return; pattern = n; paint(); if (shown) later(300); },
    changed() { if (shown) later(900); },
    mem, read, have: () => have, parts: PARTS, velAt, noteAt, set: (offset, bytes) => write(offset, bytes),
  };
})();
