const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const BASE = 0x1c00596c;
const SIZE = 0xa00;
const source = fs.readFileSync(path.join(__dirname, '../xwp1/panel/stepgrid.js'), 'utf8');

class Node {
  constructor(tag, attrs = {}, children = []) {
    this.tag = tag;
    this.children = [];
    this.events = new Map();
    this.classes = new Set((attrs.class || '').split(' ').filter(Boolean));
    this.classList = {
      toggle: (name, on) => on ? this.classes.add(name) : this.classes.delete(name),
      contains: name => this.classes.has(name),
    };
    this.style = { setProperty: (name, value) => { this.style[name] = value; } };
    this.textContent = attrs.text || '';
    this.append(...children);
  }
  append(...children) {
    for (const child of children.flat(Infinity)) if (child instanceof Node) this.children.push(child);
  }
  addEventListener(type, handler) { this.events.set(type, handler); }
  dispatch(type, extra = {}) {
    const event = { pointerId: 1, clientY: 100, button: 0, shiftKey: false,
      preventDefault() { this.defaultPrevented = true; }, ...extra };
    this.events.get(type)(event);
    return event;
  }
  setPointerCapture() {}
  getBoundingClientRect() { return { left: 10, top: 20, width: 10 }; }
  querySelectorAll(selector) {
    const [, tag, className] = selector.match(/^(\w+)\.([\w-]+)$/) || [];
    const found = [];
    const visit = node => {
      if (node.tag === tag && node.classes.has(className)) found.push(node);
      node.children.forEach(visit);
    };
    visit(this);
    return found;
  }
}

function panel() {
  const sent = [], timers = new Map();
  let touched = 0, nextTimer = 1;
  const window = { synthTaps: [], front: { touch() { touched++; } } };
  const context = { window, linked: true, sendMidi: message => sent.push(Array.from(message)),
    clearTimeout: id => timers.delete(id),
    setTimeout: (fn, delay) => { const id = nextTimer++; timers.set(id, { fn, delay }); return id; },
    clamp: (n, low, high) => Math.max(low, Math.min(high, n)),
    el: (tag, attrs = {}, ...children) => new Node(tag, attrs, children),
    field: () => new Node('div'), showTip() {}, hideTip() {} };
  vm.runInNewContext(source, context, { filename: 'stepgrid.js' });
  const grid = window.stepGrid;
  const build = () => grid.build(() => new Node('button'), () => new Node('div'), () => new Node('div'));
  const runTimer = () => {
    const [id, timer] = timers.entries().next().value;
    timers.delete(id);
    timer.fn();
  };
  return { grid, tap: window.synthTaps[0], sent, touches: () => touched,
    timers, runTimer, context, build };
}

function address(bytes) {
  return bytes.reduce((value, digit, index) => value + digit * 128 ** index, 0);
}

function reply(bytes = new Uint8Array(SIZE), at = BASE) {
  return Uint8Array.from([0xf0, 0x7d, 0x58, 0x50,
    ...Array.from({ length: 5 }, (_, i) => Math.floor(at / 128 ** i) & 127),
    ...Array.from(bytes).flatMap(byte => [byte >> 4, byte & 15]), 0xf7]);
}

test('all eight patterns keep every part and note row in separate work RAM cells', () => {
  const { grid } = panel();
  assert.deepEqual(Array.from(grid.parts, part => part.name),
    ['Drum 1', 'Drum 2', 'Drum 3', 'Drum 4', 'Drum 5', 'Bass', 'Solo 1', 'Solo 2', 'Chord']);
  const addresses = new Set();
  for (let pattern = 0; pattern < 8; pattern++) {
    grid.pattern(pattern);
    for (let part = 0; part < 9; part++) {
      const rows = part === 8 ? 3 : 1;
      for (let step = 0; step < 16; step++) {
        const cells = [grid.velAt(part, step), ...Array.from({ length: rows }, (_, row) => grid.noteAt(part, step, row))];
        for (const at of cells) {
          assert.ok(at >= 0 && at < SIZE, `out of range: ${at}`);
          assert.ok(!addresses.has(at), `overlap at ${at}`);
          addresses.add(at);
        }
      }
    }
  }
  assert.equal(addresses.size, SIZE);
  assert.equal(grid.velAt(5, 0), 7 * 32);
  assert.equal(grid.noteAt(8, 15, 2), SIZE - 1);
});

test('read asks for the whole work RAM and decodes a complete reply', () => {
  const { grid, tap, sent, context } = panel();
  grid.read();
  assert.deepEqual(sent[0].slice(0, 4), [0xf0, 0x7d, 0x58, 0x50]);
  assert.equal(address(sent[0].slice(4, 9)), BASE);
  assert.equal(address(sent[0].slice(9, 11)), SIZE);
  assert.equal(sent[0].at(-1), 0xf7);
  const bytes = Uint8Array.from({ length: SIZE }, (_, i) => i & 255);
  assert.equal(tap(reply(bytes)), true);
  assert.equal(grid.have(), true);
  assert.deepEqual(Array.from(grid.mem), Array.from(bytes));
  context.linked = false;
  grid.read();
  assert.equal(sent.length, 1);
});

test('unrelated and malformed replies leave work RAM untouched', () => {
  const { grid, tap } = panel();
  const good = reply(new Uint8Array(SIZE).fill(0xab));
  const cases = [reply(undefined, BASE + 1), good.subarray(0, good.length - 2),
    Uint8Array.from([...good, 0]), Uint8Array.from(good), Uint8Array.from(good), Uint8Array.from(good)];
  cases[3][0] = 0;
  cases[4][20] = 0x10;
  cases[5][cases[5].length - 1] = 0;
  for (const message of cases) {
    assert.equal(tap(message), false);
    assert.equal(grid.have(), false);
    assert.equal(grid.mem.some(byte => byte !== 0), false);
  }
});

test('write packs bytes into MIDI nibbles and updates local state', () => {
  const { grid, sent, touches } = panel();
  grid.set(0x123, [0, 127, 128, 255]);
  assert.deepEqual(sent[0].slice(0, 4), [0xf0, 0x7d, 0x58, 0x57]);
  assert.equal(address(sent[0].slice(4, 9)), BASE + 0x123);
  assert.deepEqual(sent[0].slice(9, -1), [0, 0, 7, 15, 8, 0, 15, 15]);
  assert.equal(sent[0].at(-1), 0xf7);
  assert.deepEqual(Array.from(grid.mem.slice(0x123, 0x127)), [0, 127, 128, 255]);
  assert.equal(touches(), 1);
});

test('visible grid coalesces refreshes and hides without a late read', () => {
  const { grid, sent, timers, runTimer, context } = panel();
  grid.changed();
  assert.equal(timers.size, 0);
  grid.show(true);
  assert.equal(sent.length, 1);
  grid.changed();
  assert.equal([...timers.values()][0].delay, 900);
  grid.pattern(2);
  assert.equal(timers.size, 1);
  assert.equal([...timers.values()][0].delay, 300);
  runTimer();
  assert.equal(sent.length, 2);
  grid.changed();
  grid.show(false);
  assert.equal(timers.size, 0);
  context.linked = false;
  grid.show(true);
  assert.equal(sent.length, 2);
});

test('step gestures toggle, tie, and drag velocity at the selected pattern', () => {
  const { grid, tap, build, sent } = panel();
  const page = build();
  const steps = page.querySelectorAll('button.sg-cell');
  assert.equal(steps.length, 9 * 16);
  assert.equal(page.classList.contains('wait'), true);
  steps[0].dispatch('pointerdown');
  steps[0].dispatch('pointerup');
  assert.equal(sent.length, 0);
  tap(reply());
  assert.equal(page.classList.contains('wait'), false);
  grid.pattern(3);
  const at = grid.velAt(0, 0), note = grid.noteAt(0, 0);
  steps[0].dispatch('pointerdown');
  steps[0].dispatch('pointerup');
  assert.equal(grid.mem[at], 0x80);
  assert.equal(steps[0].classList.contains('on'), false);
  steps[0].dispatch('pointerdown', { shiftKey: true });
  steps[0].dispatch('pointerup');
  assert.equal(grid.mem[note], 0x80);
  assert.equal(steps[0].classList.contains('tie'), true);
  steps[0].dispatch('pointerdown');
  steps[0].dispatch('pointerup');
  assert.equal(grid.mem[at], 0);
  assert.equal(grid.mem[note], 0);
  steps[0].dispatch('pointerdown');
  steps[0].dispatch('pointermove', { clientY: 80 });
  steps[0].dispatch('pointerup');
  assert.equal(grid.mem[at], 16);
  assert.equal(address(sent.at(-1).slice(4, 9)), BASE + at);
});

test('note lane edits the selected chord row and keeps the tie bit', () => {
  const { grid, tap, build, sent } = panel();
  const page = build();
  tap(reply());
  const parts = page.querySelectorAll('button.sg-part');
  const notes = page.querySelectorAll('button.sg-note');
  assert.equal(notes.length, 3 * 16);
  parts[8].dispatch('click');
  const at = grid.noteAt(8, 0, 2);
  grid.mem[at] = 0x80 | 60;
  notes[32].dispatch('wheel', { deltaY: -1, shiftKey: true });
  assert.equal(grid.mem[at], 0x80 | 72);
  assert.equal(address(sent.at(-1).slice(4, 9)), BASE + at);
  notes[32].dispatch('wheel', { deltaY: 1 });
  assert.equal(grid.mem[at], 0x80 | 71);
});
