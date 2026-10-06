// Drawn controls shared by the Hex Layer, Drawbar and PCM pages: an envelope whose corners are
// dragged, LFO and vibrato shapes, a low-pass curve, the drawbar organ's wave and spectrum.
// They use app.js's graph(), el(), watch(), vals, edit() and clamp(), and hold no state: what is
// drawn is what was read back.
'use strict';

const vizGet = (ref, fallback) => (ref && vals.get(ref.key) != null ? vals.get(ref.key) : fallback);

// An envelope drawn from offsets to the wave's own envelope. Each of attack / decay / sustain /
// release is { ref, centre, span, faster } (any may be missing): value = centre means "as the wave
// has it"; `faster` = +1 when a higher value is a quicker stage (a rate), -1 when it is a longer one
// (a time). The corners are dragged: sideways for the times, up and down for the sustain level.
function offsetEnvelope(parts, cap, cls = '') {
  const { svg, wrap } = graph(cls, cap, true);
  const fill = el('path', { class: 'fill' }), line = el('path', { class: 'line' }), ghost = el('path', { class: 'line hold' });
  const base = el('line', { class: 'grid' });
  const nodes = { attack: el('circle', { class: 'node', r: 5 }), decay: el('circle', { class: 'node', r: 5 }), release: el('circle', { class: 'node', r: 5 }) };
  svg.append(base, ghost, fill, line);
  for (const k of Object.keys(nodes)) if (parts[k] || (k === 'decay' && parts.sustain)) svg.append(nodes[k]);
  const norm = p => (p ? clamp((vizGet(p.ref, p.centre) - p.centre) / p.span, -1, 1) : 0);
  const width = (p, w) => w * 2 ** (-norm(p) * (p ? p.faster : 1) * 1.6);       // a stage's width; w at the centre
  const geometry = () => {
    const [W, H] = svg.size();
    const unit = W / 7.5, top = 10, bottom = H - 8;
    const xa = 6 + clamp(width(parts.attack, unit * 0.9), 4, unit * 2.6);
    const sustain = parts.sustain ? clamp(0.62 + 0.38 * norm(parts.sustain), 0.06, 1) : (parts.decay ? 0.62 : 1);
    const xd = xa + (parts.decay || parts.sustain ? clamp(width(parts.decay, unit * 1.3), 4, unit * 2.6) : 0);
    const xs = Math.max(xd + unit * 0.9, W * 0.62);
    const xr = xs + clamp(width(parts.release, unit * 1.2), 4, W - xs - 6);
    const y = v => bottom - v * (bottom - top);
    return { W, H, unit, top, bottom, xa, xd, xs, xr, sustain, y };
  };
  const draw = () => {
    const g = geometry();
    base.setAttribute('x1', 0); base.setAttribute('x2', g.W); base.setAttribute('y1', g.bottom); base.setAttribute('y2', g.bottom);
    const d = `M6 ${g.bottom}L${g.xa.toFixed(1)} ${g.y(1).toFixed(1)}L${g.xd.toFixed(1)} ${g.y(g.sustain).toFixed(1)}L${g.xs.toFixed(1)} ${g.y(g.sustain).toFixed(1)}L${g.xr.toFixed(1)} ${g.bottom}`;
    line.setAttribute('d', d);
    fill.setAttribute('d', d + 'Z');
    // the wave's own envelope, for comparison
    const u = g.unit, own = 6 + u * 0.9, ownD = own + (parts.decay || parts.sustain ? u * 1.3 : 0), ownS = Math.max(ownD + u * 0.9, g.W * 0.62);
    ghost.setAttribute('d', `M6 ${g.bottom}L${own} ${g.y(1)}L${ownD} ${g.y(parts.decay || parts.sustain ? 0.62 : 1)}L${ownS} ${g.y(parts.decay || parts.sustain ? 0.62 : 1)}L${ownS + u * 1.2} ${g.bottom}`);
    nodes.attack.setAttribute('cx', g.xa); nodes.attack.setAttribute('cy', g.y(1));
    nodes.decay.setAttribute('cx', g.xd); nodes.decay.setAttribute('cy', g.y(g.sustain));
    nodes.release.setAttribute('cx', g.xr); nodes.release.setAttribute('cy', g.bottom);
  };
  // a width back to the parameter's value
  const toValue = (p, w, w0) => Math.round(p.centre - p.faster * p.span * Math.log2(Math.max(w, 1) / w0) / 1.6);
  const drag = (node, move) => {
    node.addEventListener('pointerdown', e => {
      node.setPointerCapture(e.pointerId); node.classList.add('drag'); e.preventDefault();
      const at = ev => { const r = svg.getBoundingClientRect(); move(ev.clientX - r.left, ev.clientY - r.top, geometry()); };
      const stop = () => { node.classList.remove('drag'); node.removeEventListener('pointermove', at); };
      node.addEventListener('pointermove', at);
      node.addEventListener('pointerup', stop, { once: true });
      node.addEventListener('pointercancel', stop, { once: true });
    });
  };
  if (parts.attack) drag(nodes.attack, (x, y, g) => edit(parts.attack.ref, toValue(parts.attack, x - 6, g.unit * 0.9)));
  drag(nodes.decay, (x, y, g) => {
    if (parts.decay) edit(parts.decay.ref, toValue(parts.decay, x - g.xa, g.unit * 1.3));
    if (parts.sustain) edit(parts.sustain.ref, Math.round(parts.sustain.centre + parts.sustain.span * clamp(((g.bottom - y) / (g.bottom - g.top) - 0.62) / 0.38, -1, 1)));
  });
  if (parts.release) drag(nodes.release, (x, y, g) => edit(parts.release.ref, toValue(parts.release, x - g.xs, g.unit * 1.2)));
  svg.redraw = draw;
  for (const p of Object.values(parts)) if (p) watch(p.ref, draw);
  return wrap;
}

const VIZ_SHAPES = {
  sine: t => Math.sin(2 * Math.PI * t), triangle: t => 1 - 4 * Math.abs(((t + 0.25) % 1) - 0.5),
  sawUp: t => 2 * (t % 1) - 1, sawDown: t => 1 - 2 * (t % 1), square: t => ((t % 1) < 0.5 ? 1 : -1),
  pulse13: t => ((t % 1) < 0.25 ? 1 : -1), pulse31: t => ((t % 1) < 0.75 ? 1 : -1),
};

// An LFO as it comes in after a note: nothing for the delay, growing over the rise, then steady.
// o: { wave, shapes: [names by wave value], rate, rateRange: [min, max], delay, rise, depth, depthCentre, depthSpan }
function lfoGraph(o, cap, cls = 'sm') {
  const { svg, wrap } = graph(cls, cap, true);
  const line = el('path', { class: 'line' }), mid = el('line', { class: 'grid' }), swell = el('path', { class: 'fill' });
  svg.append(mid, swell, line);
  const draw = () => {
    const [W, H] = svg.size();
    mid.setAttribute('x1', 0); mid.setAttribute('x2', W); mid.setAttribute('y1', H / 2); mid.setAttribute('y2', H / 2);
    const [r0, r1] = o.rateRange || [0, 127];
    const cycles = 1.5 + clamp((vizGet(o.rate, (r0 + r1) / 2) - r0) / (r1 - r0), 0, 1) * 7;
    const depth = o.depth ? Math.abs(vizGet(o.depth, o.depthCentre) - o.depthCentre) / o.depthSpan : 0.6;
    const amp = (0.12 + clamp(depth, 0, 1) * 0.76) * (H / 2 - 4);
    const delay = o.delay ? clamp(vizGet(o.delay, 0) / 127, 0, 1) * 0.45 * W : 0;
    const rise = o.rise ? clamp(vizGet(o.rise, 0) / 127, 0, 1) * 0.4 * W : 0;
    const fn = VIZ_SHAPES[(o.shapes || ['sine'])[vizGet(o.wave, 0)] || 'sine'] || VIZ_SHAPES.sine;
    const gain = x => (x < delay ? 0 : rise > 0 ? clamp((x - delay) / rise, 0, 1) : 1);
    let d = '', last = null, top = '', bottom = '';
    for (let x = 0; x <= W; x += 1) {
      const a = amp * gain(x), v = H / 2 - a * fn(Math.max(0, x - delay) / W * cycles);
      d += (x ? (last !== null && Math.abs(v - last) > a * 0.8 && a > 1 ? `L${x} ${last.toFixed(1)}L` : 'L') : 'M') + `${x} ${v.toFixed(1)}`;
      last = v;
      top += (x ? 'L' : 'M') + `${x} ${(H / 2 - a).toFixed(1)}`;
      bottom = `L${x} ${(H / 2 + a).toFixed(1)}` + bottom;
    }
    line.setAttribute('d', d);
    swell.setAttribute('d', top + bottom + 'Z');
  };
  svg.redraw = draw;
  [o.wave, o.rate, o.delay, o.rise, o.depth].forEach(r => r && watch(r, draw));
  return wrap;
}

// The instrument's 12 dB/octave low-pass (Q 0.95, measured on the hardware) at a corner the caller
// works out from its parameter; `corner()` returns Hz, or null for "filter open".
function lowpassCurve(deps, corner, cap, cls = 'sm') {
  return curve(deps, f => {
    const fc = corner();
    if (fc == null) return 0;
    const w = f / fc, q = 0.95;
    return -10 * Math.log10((1 - w * w) ** 2 + (w / q) ** 2) - 1;
  }, cap, cls, [-36, 9]);
}

// The drawbar organ's tone: two periods of the 16' partial's wave.
// positions: refs in the instrument's order 16', 8', 4', 2', 1', 5 1/3', 2 2/3', 1 3/5', 1 1/3'.
const ORGAN_CYCLES = [1, 2, 4, 8, 16, 3, 6, 10, 12];                       // periods per period of the 16'
const ORGAN_LEVEL_DB = [-99, -15.2, -11.0, -8.1, -5.8, -4.1, -2.7, -1.6, 0]; // by position 0..8, measured
function organWave(positions, cap) {
  const { svg, wrap } = graph('', cap, true);
  const line = el('path', { class: 'line' }), fill = el('path', { class: 'fill' }), mid = el('line', { class: 'grid' });
  svg.append(mid, fill, line);
  const draw = () => {
    const [W, H] = svg.size();
    const amps = positions.map(ref => { const p = vizGet(ref, 0); return p > 0 ? 10 ** (ORGAN_LEVEL_DB[clamp(p, 0, 8)] / 20) : 0; });
    const waveW = W, h = H / 2 - 6;
    mid.setAttribute('x1', 0); mid.setAttribute('x2', waveW); mid.setAttribute('y1', H / 2); mid.setAttribute('y2', H / 2);
    const wave = [];
    for (let x = 0; x <= waveW; x += 1) {
      const t = x / waveW * 2;      // two periods
      let v = 0;
      for (let i = 0; i < 9; i++) v += amps[i] * Math.sin(2 * Math.PI * ORGAN_CYCLES[i] * t);
      wave.push(v);
    }
    // scaled to its own peak so it never clips; a quiet setting (one drawbar barely out) still looks small
    const total = Math.max(0.45, ...wave.map(Math.abs));
    let d = '';
    wave.forEach((v, x) => { d += (x ? 'L' : 'M') + `${x} ${(H / 2 - v / total * h).toFixed(1)}`; });
    line.setAttribute('d', d);
    fill.setAttribute('d', d + `L${waveW} ${H / 2}L0 ${H / 2}Z`);
  };
  svg.redraw = draw;
  positions.forEach(r => watch(r, draw));
  return wrap;
}

// A percussive decay: how long the organ's percussion rings (0..127).
function decayCurve(time, on, cap) {
  const { svg, wrap } = graph('sm', cap, true);
  const line = el('path', { class: 'line' }), fill = el('path', { class: 'fill' });
  svg.append(fill, line);
  const draw = () => {
    const [W, H] = svg.size();
    const active = !on || vizGet(on, 0) > 0;
    const tau = (0.05 + vizGet(time, 30) / 127 * 0.6) * W;
    let d = `M4 ${H - 6}L8 10`;
    for (let x = 8; x <= W; x += 2) d += `L${x} ${(H - 6 - (H - 16) * Math.exp(-(x - 8) / tau)).toFixed(1)}`;
    line.setAttribute('d', d);
    fill.setAttribute('d', d + `L${W} ${H - 6}Z`);
    svg.style.opacity = active ? 1 : 0.35;
  };
  svg.redraw = draw;
  [time, on].forEach(r => r && watch(r, draw));
  return wrap;
}

// ---------------------------------------------------------------- wave pictures

// The instrument's own samples as small pictures (tools/panel_waves.py: waves.json / waves.bin,
// made from the firmware on this machine). Without the files the pictures are left out.
const wavePics = { index: null, bin: null, waiting: [] };
(async () => {
  try {
    const [index, bin] = await Promise.all([fetch('waves.json').then(r => r.ok ? r.json() : null), fetch('waves.bin').then(r => r.ok ? r.arrayBuffer() : null)]);
    if (!index || !bin) return;
    wavePics.index = index;
    wavePics.bin = new Int8Array(bin);
    wavePics.waiting.splice(0).forEach(draw => draw());
  } catch (e) { /* no pictures */ }
})();

// block: 'synth' | 'pcm' | 'noise' (the Hex Layer's waves are PCM waves), wave: its place in that
// block's list. Many PCM waves are several samples split by key: this is the one `key` plays.
function waveShot(block, wave, key = 60) {
  const ix = wavePics.index, splits = ix && ix.blocks[block] && ix.blocks[block][wave];
  if (!splits || !splits.length) return null;
  let i = 0;
  while (i + 1 < splits.length && splits[i + 1][0] <= key) i++;
  const [length, loop, , span, inc, at, track = 1] = ix.shots[splits[i][1]], n = ix.n, o = splits[i][1] * 3 * n;
  const rate = 42818.1 * inc * 2 ** ((key - at) * track / 12);       // stored samples a second at `key`
  return { n, lo: wavePics.bin.subarray(o, o + n), hi: wavePics.bin.subarray(o + n, o + 2 * n), shape: wavePics.bin.subarray(o + 2 * n, o + 3 * n),
           key, length, loop, span, seconds: length / rate, loopSeconds: (length - loop) / rate,
           part: i, parts: splits.length, from: i ? splits[i][0] : null, to: i + 1 < splits.length ? splits[i + 1][0] - 1 : null };
}

function waveText(s) {
  const secs = t => t >= 1 ? `${t.toFixed(t < 10 ? 1 : 0)} s` : `${Math.max(1, Math.round(t * 1000))} ms`;
  const parts = [];
  if (s.length - s.loop === s.span && s.loop < 64) parts.push(`${s.span} samples, repeating`);        // a synth wave: all loop
  else parts.push(`${secs(s.seconds)} at ${noteName(s.key)}`, s.loop < s.length ? `then repeats its last ${secs(s.loopSeconds)}` : 'plays once');
  if (s.parts > 1) parts.push(`sample ${s.part + 1} of ${s.parts} across the keys (${s.from == null ? `up to ${noteName(s.to)}` : s.to == null ? `from ${noteName(s.from)}` : `${noteName(s.from)}–${noteName(s.to)}`})`);
  return parts.join(' · ');
}

// The picture of a wave: the whole sample (with the part that repeats marked), and beside it the
// shape that repeats. A short looping sample (the synth waves) is drawn as the shape alone.
// get() -> [block, wave, key] or null (key: which of a split wave's samples, default C4); cls 'big'
// for the tall one with a caption.
function wavePicture(get, cls = '') {
  const big = cls.includes('big');
  const { svg, wrap } = graph('wavepic ' + cls, big ? ' ' : '', true);
  const cap = wrap.querySelector('.cap');
  const mid = el('line', { class: 'grid' }), tint = el('rect', { class: 'tint' }), mark = el('line', { class: 'mark' }),
    body = el('path', { class: 'body' }), line = el('path', { class: 'line' });
  svg.append(tint, mid, body, mark, line);
  const draw = () => {
    const pick = wavePics.index && get(), s = pick && waveShot(pick[0], pick[1], pick[2] ?? 60);
    wrap.hidden = !wavePics.index;
    svg.classList.toggle('none', !s);
    for (const node of [tint, mark, body, line]) node.setAttribute('visibility', 'hidden');
    const [W, H] = svg.size(), top = big ? 8 : 4, h = H - top - (big ? 22 : 4), y = v => (top + h / 2 - v / 127 * h / 2).toFixed(1);
    mid.setAttribute('x1', 0); mid.setAttribute('x2', W); mid.setAttribute('y1', top + h / 2); mid.setAttribute('y2', top + h / 2);
    wrap.title = s ? waveText(s) : '';
    if (cap) cap.textContent = s ? waveText(s) : '';
    if (!s) return;
    const looped = s.loop < s.length, whole = !(looped && s.length < 1500);
    const gap = whole && looped ? 8 : 0, wholeW = !whole ? 0 : looped ? Math.round((W - gap) * 0.6) : W, x0 = wholeW + gap;
    if (whole) {
      let d = '';
      for (let i = 0; i < s.n; i++) d += (i ? 'L' : 'M') + `${(i / (s.n - 1) * wholeW).toFixed(1)} ${y(s.hi[i])}`;
      for (let i = s.n - 1; i >= 0; i--) d += `L${(i / (s.n - 1) * wholeW).toFixed(1)} ${y(s.lo[i])}`;
      body.setAttribute('d', d + 'Z');
      body.setAttribute('visibility', 'visible');
      if (looped) {
        const x = s.loop / s.length * wholeW;
        tint.setAttribute('x', x); tint.setAttribute('y', top); tint.setAttribute('width', wholeW - x); tint.setAttribute('height', h);
        mark.setAttribute('x1', x); mark.setAttribute('x2', x); mark.setAttribute('y1', top); mark.setAttribute('y2', top + h);
        tint.setAttribute('visibility', 'visible'); mark.setAttribute('visibility', 'visible');
      }
    }
    if (looped) {
      let d = '';
      for (let i = 0; i < s.n; i++) d += (i ? 'L' : 'M') + `${(x0 + i / (s.n - 1) * (W - x0)).toFixed(1)} ${y(s.shape[i] * (whole ? 0.8 : 0.9))}`;
      line.setAttribute('d', d);
      line.setAttribute('visibility', 'visible');
    }
  };
  svg.redraw = draw;
  if (!wavePics.index) { wrap.hidden = true; wavePics.waiting.push(draw); }
  return { wrap, draw };
}
