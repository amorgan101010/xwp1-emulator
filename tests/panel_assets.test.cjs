const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const panel = path.join(__dirname, '../xwp1/panel');
const html = fs.readFileSync(path.join(panel, 'index.html'), 'utf8');
const css = fs.readFileSync(path.join(panel, 'style.css'), 'utf8');

test('every local script and stylesheet in the page exists', () => {
  for (const [, name] of html.matchAll(/<(?:script|link)\b[^>]*\b(?:src|href)="([^"]+)"/g)) {
    assert.ok(fs.existsSync(path.join(panel, name)), `${name} is missing`);
  }
});

test('every local font referenced by the stylesheet exists', () => {
  let checked = 0;
  for (const [, name] of css.matchAll(/url\((?:["']?)(fonts\/[^)'" ]+)(?:["']?)\)/g)) {
    assert.ok(fs.existsSync(path.join(panel, name)), `${name} is missing`);
    checked++;
  }
  assert.ok(checked > 0, 'no font references found');
});

test('panel data files fetched by scripts have static definitions or local generated data', () => {
  const scripts = fs.readdirSync(panel).filter(name => name.endsWith('.js'));
  const fetched = new Set();
  for (const script of scripts) {
    const source = fs.readFileSync(path.join(panel, script), 'utf8');
    for (const [, name] of source.matchAll(/fetch\(['"]([^'"]+\.json)['"]\)/g)) fetched.add(name);
  }
  // app.js also fetches `${engine}_mem.json` for each non-solo tone engine.
  for (const engine of ['hex', 'draw', 'pcm']) fetched.add(`${engine}_mem.json`);
  assert.ok(fetched.size > 3, 'no static data fetches found');
  for (const name of fetched) {
    const generated = path.join(panel, name);
    if (name === 'waves.json' && !fs.existsSync(generated)) continue;
    const source = path.join(__dirname, '../xwp1/assets', name);
    const file = fs.existsSync(generated) ? generated : source;
    assert.ok(fs.existsSync(file), `${name} is missing`);
    assert.doesNotThrow(() => JSON.parse(fs.readFileSync(file, 'utf8')), `${name} is invalid JSON`);
  }
});

test('effect definitions have distinct IDs and usable parameter labels', () => {
  const { types } = JSON.parse(fs.readFileSync(path.join(panel, 'dsp.json'), 'utf8'));
  assert.ok(Array.isArray(types) && types.length > 0);
  const ids = new Set();
  for (const effect of types) {
    assert.ok(Number.isInteger(effect.id) && effect.id > 0);
    assert.ok(!ids.has(effect.id), `duplicate effect ID ${effect.id}`);
    ids.add(effect.id);
    assert.ok(typeof effect.name === 'string' && effect.name.length > 0);
    assert.ok(Array.isArray(effect.params) && effect.params.every(label => typeof label === 'string' && label.length > 0));
  }
});
