// vps-watcher web UI. Plain JS, no dependencies. All server text goes through textContent.
'use strict';

/* ---------- tiny DOM helpers ---------- */

/** Creates an element: h('div', {class: 'x', onclick: fn}, child, 'text'). */
function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'class') el.className = v;
    else if (k === 'text') el.textContent = v;
    else if (k === 'style') Object.assign(el.style, v);
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat()) {
    if (kid === null || kid === undefined || kid === false) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

/** Replaces an element's children, skipping null/false like h() does. */
function fill(el, ...kids) {
  el.replaceChildren(...kids.flat().filter((k) => k !== null && k !== undefined && k !== false));
  return el;
}

const ICONS = {
  overview: '<rect x="3" y="3" width="7" height="9" rx="1"/><rect x="14" y="3" width="7" height="5" rx="1"/><rect x="14" y="12" width="7" height="9" rx="1"/><rect x="3" y="16" width="7" height="5" rx="1"/>',
  apps: '<path d="M21 8 12 3 3 8v8l9 5 9-5z"/><path d="m3 8 9 5 9-5M12 13v8"/>',
  logs: '<path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"/><path d="M14 3v6h6M8 13h8M8 17h5"/>',
  system: '<rect x="5" y="5" width="14" height="14" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3"/>',
  monitors: '<path d="M22 12h-4l-3 8L9 4l-3 8H2"/>',
  alerts: '<path d="M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9"/><path d="M13.7 21a2 2 0 0 1-3.4 0"/>',
  send: '<path d="m22 2-7 20-4-9-9-4z"/><path d="M22 2 11 13"/>',
  reports: '<path d="M3 3v18h18"/><path d="M7 15v3M12 10v8M17 6v12"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h0a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v0a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
  menu: '<path d="M4 6h16M4 12h16M4 18h16"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
  moon: '<path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/>',
  out: '<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9"/>',
  refresh: '<path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/>',
  download: '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/>',
  printer: '<path d="M6 9V2h12v7M6 18H4a2 2 0 0 1-2-2v-5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2h-2"/><rect x="6" y="14" width="12" height="8"/>',
  trash: '<path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/>',
  back: '<path d="M19 12H5M12 19l-7-7 7-7"/>',
};

/** Inline SVG icon from the fixed set above (static markup, never user data). */
function icon(name) {
  const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  s.setAttribute('viewBox', '0 0 24 24');
  s.setAttribute('class', 'icon');
  s.setAttribute('aria-hidden', 'true');
  s.innerHTML = ICONS[name] || '';
  return s;
}

/* ---------- formatting ---------- */

const fmt = {
  bytes(n) {
    if (n === null || n === undefined || !isFinite(n)) return '–';
    const u = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
    let i = 0;
    while (Math.abs(n) >= 1024 && i < u.length - 1) { n /= 1024; i++; }
    return (i === 0 ? Math.round(n) : n.toFixed(Math.abs(n) < 10 ? 1 : 0)) + ' ' + u[i];
  },
  rate(n) { return n === null || n === undefined ? '–' : fmt.bytes(n) + '/s'; },
  pct(n) {
    if (n === null || n === undefined || !isFinite(n)) return '–';
    return (n < 10 ? n.toFixed(1) : Math.round(n)) + '%';
  },
  num(n, d = 1) { return n === null || n === undefined || !isFinite(n) ? '–' : Number(n).toFixed(d); },
  dur(s) {
    if (!s && s !== 0) return '–';
    const d = Math.floor(s / 86400), hh = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
    if (d) return `${d}d ${hh}h`;
    if (hh) return `${hh}h ${m}m`;
    return m ? `${m}m` : `${Math.floor(s)}s`;
  },
  ago(ts) {
    const s = Math.max(0, Math.round(Date.now() / 1000 - ts));
    return s < 5 ? 'just now' : fmt.dur(s) + ' ago';
  },
  time(ts, withDate) {
    const d = new Date(ts * 1000);
    const t = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    return withDate ? d.toLocaleDateString([], { month: 'short', day: 'numeric' }) + ' ' + t : t;
  },
  stamp(ts) {
    return new Date(ts * 1000).toLocaleString([], { month: 'short', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' });
  },
};

/** Value formatter for a unit name used by charts and tables. */
function formatter(unit) {
  if (unit === '%') return fmt.pct;
  if (unit === 'B') return fmt.bytes;
  if (unit === 'B/s') return fmt.rate;
  if (unit === 'ms') return (v) => fmt.num(v, 0) + ' ms';
  return (v) => fmt.num(v, 2);
}

/* ---------- API ---------- */

async function call(method, path, body) {
  const opt = { method, headers: {}, credentials: 'same-origin' };
  if (body !== undefined) {
    opt.headers['content-type'] = 'application/json';
    opt.body = JSON.stringify(body);
  }
  let r;
  try {
    r = await fetch(path, opt);
  } catch {
    throw new Error('Cannot reach vps-watcher. Is the service running?');
  }
  let data = null;
  try { data = await r.json(); } catch { /* empty body */ }
  if (r.status === 401 && !['/api/login', '/api/session', '/api/account'].includes(path)) {
    boot();
    throw new Error('Signed out.');
  }
  if (!r.ok) throw new Error((data && data.error) || `Request failed (${r.status})`);
  return data;
}
const api = {
  get: (p) => call('GET', p),
  post: (p, b) => call('POST', p, b || {}),
  del: (p) => call('DELETE', p),
};

/* ---------- state & storage ---------- */

const store = {
  get(k, d) { try { return localStorage.getItem('vpsw.' + k) ?? d; } catch { return d; } },
  set(k, v) { try { localStorage.setItem('vpsw.' + k, v); } catch { /* private mode */ } },
};

const state = {
  session: null,
  snap: null,
  lastAt: 0,
  live: null,
  onSnap: null,
  cleanup: [],
  range: store.get('range', '1h'),
};

function applyTheme() {
  const t = store.get('theme', 'system');
  if (t === 'system') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.setAttribute('data-theme', t);
}

function toast(msg) {
  const t = h('div', { class: 'toast', role: 'status', text: msg });
  document.body.append(t);
  setTimeout(() => t.remove(), 2600);
}

/** Reads a CSS custom property, so charts follow the theme. */
function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/* ---------- charts (canvas, crosshair + tooltip, keyboard) ---------- */

class LineChart {
  /** opts: {unit, max, series: [{label, color: '--s1'}], label} */
  constructor(opts) {
    this.o = opts;
    this.el = h('div', { class: 'chart', tabindex: '0', role: 'img', 'aria-label': opts.label || 'chart' });
    this.canvas = h('canvas');
    this.tip = h('div', { class: 'tip', hidden: true });
    this.empty = h('div', { class: 'chart-empty', text: 'Collecting data…', hidden: true });
    this.el.append(this.canvas, this.tip, this.empty);
    this.data = null;
    this.hover = null;
    this.el.addEventListener('pointermove', (e) => this.move(e.offsetX));
    this.el.addEventListener('pointerleave', () => this.setHover(null));
    this.el.addEventListener('blur', () => this.setHover(null));
    this.el.addEventListener('keydown', (e) => this.key(e));
    this.ro = new ResizeObserver(() => this.draw());
    this.ro.observe(this.el);
    state.cleanup.push(() => this.ro.disconnect());
  }

  /** start/step in seconds; arrays: one values array per series (null = gap). */
  set(start, step, arrays) {
    this.data = { start, step, arrays };
    this.el.classList.remove('loading');
    const n = arrays.reduce((m, a) => Math.max(m, a.length), 0);
    const any = arrays.some((a) => a.some((v) => v !== null));
    this.empty.hidden = any;
    this.n = n;
    if (any) {
      const f = formatter(this.o.unit);
      const last = arrays.map((a, i) => `${this.o.series[i].label} ${f(lastKnown(a))}`).join(', ');
      this.el.setAttribute('aria-label', `${this.o.label}. Latest: ${last}`);
    }
    this.draw();
  }

  geom() {
    const w = this.el.clientWidth, hgt = this.el.clientHeight;
    return { w, h: hgt, l: this.left || 52, r: 10, t: 10, b: 22 };
  }

  draw() {
    const c = this.canvas, g = this.geom();
    if (!g.w || !g.h) return;
    const dpr = window.devicePixelRatio || 1;
    c.width = Math.round(g.w * dpr);
    c.height = Math.round(g.h * dpr);
    const ctx = c.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, g.w, g.h);
    if (!this.data || !this.n) return;
    const { start, step, arrays } = this.data;
    const pw = g.w - g.l - g.r, ph = g.h - g.t - g.b;
    let max = this.o.max || 0;
    if (!this.o.max) for (const a of arrays) for (const v of a) if (v !== null && v > max) max = v;
    if (this.o.unit === 'B/s' || this.o.unit === 'B') max = Math.max(max, 1024);
    max = niceMax(max || 1);
    this.max = max;
    const x = (i) => g.l + (this.n > 1 ? (i / (this.n - 1)) * pw : pw / 2);
    const y = (v) => g.t + ph - (v / max) * ph;
    const f = formatter(this.o.unit);

    ctx.font = '11px ' + cssVar('--sans');
    // Size the y-axis gutter to its widest label so values are never clipped.
    const want = Math.ceil(Math.max(...[0, 1, 2, 3, 4].map((i) => ctx.measureText(f((max / 4) * i)).width))) + 14;
    if (want !== this.left) { this.left = want; return this.draw(); }
    ctx.textBaseline = 'middle';
    ctx.lineWidth = 1;
    for (let i = 0; i <= 4; i++) {
      const v = (max / 4) * i, yy = Math.round(y(v)) + 0.5;
      ctx.strokeStyle = i === 0 ? cssVar('--axis') : cssVar('--grid');
      ctx.beginPath(); ctx.moveTo(g.l, yy); ctx.lineTo(g.w - g.r, yy); ctx.stroke();
      ctx.fillStyle = cssVar('--muted');
      ctx.textAlign = 'right';
      ctx.fillText(f(v), g.l - 8, yy);
    }
    const span = step * (this.n - 1);
    const ticks = Math.max(2, Math.min(6, Math.floor(pw / 90)));
    ctx.textAlign = 'center';
    ctx.textBaseline = 'alphabetic';
    for (let i = 0; i <= ticks; i++) {
      const idx = Math.round((i / ticks) * (this.n - 1));
      const ts = start + idx * step;
      ctx.fillText(fmt.time(ts, span > 86400 * 1.5), Math.min(Math.max(x(idx), g.l + 20), g.w - g.r - 20), g.h - 6);
    }

    arrays.forEach((a, si) => {
      const color = cssVar(this.o.series[si].color);
      ctx.strokeStyle = color;
      ctx.lineWidth = 2;
      ctx.lineJoin = 'round';
      let run = [];
      const flush = () => {
        if (!run.length) return;
        if (arrays.length === 1 && run.length > 1) {
          ctx.beginPath();
          ctx.moveTo(x(run[0][0]), y(0));
          for (const [i, v] of run) ctx.lineTo(x(i), y(v));
          ctx.lineTo(x(run[run.length - 1][0]), y(0));
          ctx.closePath();
          ctx.globalAlpha = 0.12;
          ctx.fillStyle = color;
          ctx.fill();
          ctx.globalAlpha = 1;
        }
        ctx.beginPath();
        run.forEach(([i, v], k) => (k ? ctx.lineTo(x(i), y(v)) : ctx.moveTo(x(i), y(v))));
        if (run.length === 1) ctx.arc(x(run[0][0]), y(run[0][1]), 1.5, 0, Math.PI * 2);
        ctx.stroke();
        run = [];
      };
      a.forEach((v, i) => (v === null ? flush() : run.push([i, Math.min(v, max)])));
      flush();
    });

    if (this.hover !== null) {
      const xx = Math.round(x(this.hover)) + 0.5;
      ctx.strokeStyle = cssVar('--axis');
      ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(xx, g.t); ctx.lineTo(xx, g.t + ph); ctx.stroke();
      arrays.forEach((a, si) => {
        const v = a[this.hover];
        if (v === null || v === undefined) return;
        ctx.fillStyle = cssVar(this.o.series[si].color);
        ctx.strokeStyle = cssVar('--surface');
        ctx.lineWidth = 2;
        ctx.beginPath(); ctx.arc(xx, y(Math.min(v, max)), 4, 0, Math.PI * 2); ctx.fill(); ctx.stroke();
      });
    }
  }

  move(px) {
    if (!this.data || !this.n) return;
    const g = this.geom();
    const i = Math.round(((px - g.l) / (g.w - g.l - g.r)) * (this.n - 1));
    this.setHover(Math.max(0, Math.min(this.n - 1, i)));
  }

  key(e) {
    if (!this.n) return;
    const d = { ArrowLeft: -1, ArrowRight: 1 }[e.key];
    if (!d) return;
    e.preventDefault();
    this.setHover(Math.max(0, Math.min(this.n - 1, (this.hover ?? this.n - 1) + d)));
  }

  setHover(i) {
    this.hover = i;
    this.draw();
    if (i === null) { this.tip.hidden = true; return; }
    const { start, step, arrays } = this.data;
    const f = formatter(this.o.unit);
    fill(this.tip, 
      h('div', { class: 't', text: fmt.stamp(start + i * step) }),
      ...arrays.map((a, si) => h('div', { class: 'r' },
        h('i', { style: { background: cssVar(this.o.series[si].color) } }),
        h('b', { text: a[i] === null || a[i] === undefined ? 'no data' : f(a[i]) }),
        arrays.length > 1 ? h('span', { class: 'muted', text: this.o.series[si].label }) : null)),
    );
    this.tip.hidden = false;
    const g = this.geom();
    const xx = g.l + (i / Math.max(1, this.n - 1)) * (g.w - g.l - g.r);
    const tw = this.tip.offsetWidth;
    this.tip.style.left = (xx + 12 + tw > g.w ? xx - tw - 12 : xx + 12) + 'px';
    this.tip.style.top = '8px';
  }
}

function lastKnown(a) {
  for (let i = a.length - 1; i >= 0; i--) if (a[i] !== null) return a[i];
  return null;
}

/** Rounds a chart maximum up to 1, 2, 2.5 or 5 × 10^n. */
function niceMax(v) {
  const p = Math.pow(10, Math.floor(Math.log10(v)));
  for (const m of [1, 2, 2.5, 5, 10]) if (v <= m * p) return m * p;
  return 10 * p;
}

/** Tiny line without axes for KPI tiles. */
function sparkline(canvas, values, color, max) {
  const w = canvas.clientWidth, hh = canvas.clientHeight;
  if (!w || !hh) return;
  const dpr = window.devicePixelRatio || 1;
  canvas.width = w * dpr; canvas.height = hh * dpr;
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const vals = values.filter((v) => v !== null);
  if (vals.length < 2) return;
  const top = max || Math.max(...vals) * 1.15 || 1;
  ctx.strokeStyle = cssVar(color);
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  let started = false;
  values.forEach((v, i) => {
    if (v === null) { started = false; return; }
    const x = (i / (values.length - 1)) * w, y = hh - 2 - (Math.min(v, top) / top) * (hh - 4);
    if (started) ctx.lineTo(x, y); else ctx.moveTo(x, y);
    started = true;
  });
  ctx.stroke();
}

/** Fetches history for keys and fills the given charts. specs: [{chart, keys:[...]}] */
async function loadCharts(specs, range) {
  const keys = [...new Set(specs.flatMap((s) => s.keys))];
  if (!keys.length) return;
  specs.forEach((s) => s.chart.el.classList.add('loading'));
  try {
    const d = await api.get(`/api/history?range=${range}&keys=${encodeURIComponent(keys.join(','))}`);
    for (const s of specs) {
      const series = s.keys.map((k) => d.series[k]);
      const ref = series.find(Boolean);
      if (!ref) { s.chart.set(0, 1, s.keys.map(() => [])); continue; }
      s.chart.set(ref.start, ref.step, series.map((x) => (x ? x.values : [])));
    }
  } catch (e) {
    toast(e.message);
  }
}

/** Segmented control for 1h / 24h / 7d / 30d. */
function rangePicker(current, onChange) {
  const seg = h('div', { class: 'seg', role: 'group', 'aria-label': 'Time range' });
  for (const r of ['1h', '24h', '7d', '30d']) {
    const b = h('button', { type: 'button', 'aria-pressed': String(r === current), text: r });
    b.addEventListener('click', () => {
      seg.querySelectorAll('button').forEach((x) => x.setAttribute('aria-pressed', String(x === b)));
      onChange(r);
    });
    seg.append(b);
  }
  return seg;
}

function legend(series) {
  return h('div', { class: 'legend' }, series.map((s) => h('span', {}, h('i', { style: { background: cssVar(s.color) } }), s.label)));
}

/** A card holding one chart, with title and optional legend. */
function chartCard(title, opts) {
  const chart = new LineChart({ ...opts, label: title });
  const card = h('section', { class: 'card' },
    h('div', { class: 'card-head' }, h('h2', { text: title }), opts.series.length > 1 ? legend(opts.series) : null),
    chart.el);
  return { card, chart };
}

/* ---------- small widgets ---------- */

function stateBadge(app) {
  if (app.state === 'running') {
    if (app.health === 'unhealthy') return h('span', { class: 'badge bad', text: 'unhealthy' });
    if (app.health === 'starting') return h('span', { class: 'badge warn', text: 'starting' });
    return h('span', { class: 'badge good', text: app.health === 'healthy' ? 'healthy' : 'running' });
  }
  if (app.state === 'failed' || app.state === 'dead') return h('span', { class: 'badge bad', text: app.state });
  if (app.state === 'restarting') return h('span', { class: 'badge warn', text: 'restarting' });
  return h('span', { class: 'badge', text: app.state || 'unknown' });
}

function upBadge(up) {
  if (up === true) return h('span', { class: 'badge good', text: 'up' });
  if (up === false) return h('span', { class: 'badge bad', text: 'down' });
  return h('span', { class: 'badge', text: 'pending' });
}

/** Thin usage bar that turns amber at 80% and red at 90%. */
function meter(pct) {
  const m = h('div', { class: 'meter' + (pct >= 90 ? ' bad' : pct >= 80 ? ' warn' : ''), role: 'meter', 'aria-valuenow': String(Math.round(pct)), 'aria-valuemin': '0', 'aria-valuemax': '100' });
  m.append(h('i', { style: { width: Math.min(100, Math.max(0, pct)) + '%' } }));
  return m;
}

/** Sortable table. cols: [{key, label, num, get(row), render(row)}]. */
function sortableTable(cols, opts = {}) {
  const sortKey = 'sort.' + (opts.id || 't');
  let [sk, sd] = (store.get(sortKey, opts.defaultSort || cols[0].key + ':desc')).split(':');
  const tbody = h('tbody');
  const ths = cols.map((c) => {
    const th = h('th', { class: c.num ? 'n' : '', text: c.label, scope: 'col' });
    if (c.get) {
      th.dataset.sort = c.key;
      th.tabIndex = 0;
      const act = () => {
        sd = sk === c.key && sd === 'desc' ? 'asc' : 'desc';
        sk = c.key;
        store.set(sortKey, sk + ':' + sd);
        paint();
      };
      th.addEventListener('click', act);
      th.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); act(); } });
    }
    return th;
  });
  const table = h('table', {}, h('thead', {}, h('tr', {}, ths)), tbody);
  let rows = [];
  function paint() {
    ths.forEach((th, i) => th.setAttribute('aria-sort', cols[i].key === sk ? (sd === 'asc' ? 'ascending' : 'descending') : 'none'));
    const col = cols.find((c) => c.key === sk && c.get);
    const sorted = col ? [...rows].sort((a, b) => {
      const x = col.get(a), y = col.get(b);
      const r = typeof x === 'string' || typeof y === 'string' ? String(x ?? '').localeCompare(String(y ?? '')) : (x ?? -1) - (y ?? -1);
      return sd === 'asc' ? r : -r;
    }) : rows;
    fill(tbody, ...sorted.map((r) => {
      const tr = h('tr', { class: opts.onRow ? 'link' : '' }, cols.map((c) => h('td', { class: c.num ? 'n' : '' }, c.render ? c.render(r) : String(c.get(r) ?? '–'))));
      if (opts.onRow) {
        tr.tabIndex = 0;
        tr.addEventListener('click', () => opts.onRow(r));
        tr.addEventListener('keydown', (e) => { if (e.key === 'Enter') opts.onRow(r); });
      }
      return tr;
    }));
    if (!sorted.length && opts.empty) tbody.append(h('tr', {}, h('td', { colspan: String(cols.length), class: 'empty-state', text: opts.empty })));
  }
  return { el: h('div', { class: 'table-wrap' }, table), set(r) { rows = r; paint(); } };
}

function nameCell(title, sub) {
  return h('div', { class: 'name-cell' }, h('b', { text: title }), sub ? h('span', { text: sub, title: sub }) : null);
}

/* ---------- live updates ---------- */

function startLive() {
  stopLive();
  if (document.hidden) return;
  const es = new EventSource('/api/live');
  es.onmessage = (e) => {
    try { state.snap = JSON.parse(e.data); } catch { return; }
    state.lastAt = Date.now();
    updateChrome();
    if (state.onSnap) state.onSnap(state.snap);
  };
  es.onerror = () => {
    // The browser retries on its own; check whether we were signed out meanwhile.
    if (es.readyState === EventSource.CLOSED) setTimeout(boot, 3000);
  };
  state.live = es;
}

function stopLive() {
  if (state.live) state.live.close();
  state.live = null;
}

document.addEventListener('visibilitychange', () => {
  if (!state.session || !$id('main')) return;
  if (document.hidden) stopLive();
  else { refreshSnap(); startLive(); }
});

async function refreshSnap() {
  try {
    state.snap = await api.get('/api/overview');
    state.lastAt = Date.now();
    updateChrome();
    if (state.onSnap) state.onSnap(state.snap);
  } catch { /* shown by live indicator */ }
}

function $id(id) { return document.getElementById(id); }

/** Updates the live dot and footer usage every second. */
function updateChrome() {
  const live = $id('live');
  if (!live) return;
  const s = state.snap;
  const age = (Date.now() - state.lastAt) / 1000;
  const fresh = s && age < Math.max(15, (s.interval || 5) * 3);
  live.className = 'live ' + (fresh ? 'on' : 'stale');
  live.lastChild.textContent = !s ? 'Connecting…' : fresh ? 'Live' : `Stale · ${fmt.dur(Math.round(age))}`;
  if (s) {
    $id('host-name').textContent = s.hostname || 'server';
    $id('own').textContent = `${fmt.bytes(s.own.rss)} RAM · ${fmt.pct(s.own.cpu_pct)} CPU`;
    const n = (s.alerts || []).length;
    const c = $id('alert-count');
    if (c) { c.hidden = !n; c.textContent = String(n); c.classList.toggle('warn', n > 0 && s.alerts.every((a) => a.severity !== 'critical')); }
  }
}
setInterval(updateChrome, 1000);

/* ---------- boot & routing ---------- */

const PAGES = [
  ['overview', 'Overview'],
  ['apps', 'Apps'],
  ['logs', 'Logs'],
  ['system', 'System'],
  ['monitors', 'Monitors'],
  ['alerts', 'Alerts'],
  ['reports', 'Reports'],
  ['settings', 'Settings'],
];

async function boot() {
  applyTheme();
  stopLive();
  const app = $id('app');
  try {
    state.session = await api.get('/api/session');
  } catch (e) {
    fill(app, h('div', { class: 'auth' }, h('div', { class: 'card' }, h('p', { class: 'error', text: e.message }), h('button', { onclick: boot, text: 'Retry' }))));
    return;
  }
  const s = state.session;
  if (s.setup_required) return renderSetup(app);
  if (!s.authenticated && (s.mode === 'login' || location.hash === '#/login')) return renderLogin(app);
  renderShell(app);
  await refreshSnap();
  startLive();
  route();
}

function parseHash() {
  const raw = location.hash.replace(/^#\/?/, '');
  const [path, query] = raw.split('?');
  const parts = path.split('/').map(decodeURIComponent);
  return { page: parts[0] || 'overview', arg: parts.slice(1).join('/'), q: new URLSearchParams(query || '') };
}

function route() {
  const main = $id('main');
  if (!main) return;
  state.cleanup.forEach((f) => f());
  state.cleanup = [];
  state.onSnap = null;
  const { page, arg, q } = parseHash();
  if (page === 'login') return boot();
  const isAdminPage = page === 'settings' && !state.session.authenticated;
  document.querySelectorAll('.nav a').forEach((a) => a.toggleAttribute('aria-current', false));
  const navKey = page === 'app' ? 'apps' : page;
  const cur = document.querySelector(`.nav a[data-page="${navKey}"]`);
  if (cur) cur.setAttribute('aria-current', 'page');
  $id('side').classList.remove('open');
  const view = h('div');
  fill(main, view);
  const render = {
    overview: pageOverview, apps: pageApps, app: () => pageApp(view, arg), logs: () => pageLogs(view, q),
    system: () => pageSystem(view, q), monitors: pageMonitors, alerts: pageAlerts, reports: pageReports, settings: pageSettings,
  }[page];
  if (!render || isAdminPage) return pageTitle(view, 'Not found', h('p', { class: 'muted', text: 'This page does not exist.' }));
  render(view, arg);
  window.scrollTo(0, 0);
}
window.addEventListener('hashchange', route);

/** Page header with title, extra controls, live indicator. */
function pageTitle(view, title, ...extra) {
  document.title = `${title} · vps-watcher`;
  const head = h('div', { class: 'topbar' },
    h('button', { class: 'icon-btn menu-btn', 'aria-label': 'Open menu', onclick: () => $id('side').classList.toggle('open') }, icon('menu')),
    h('h1', { text: title }));
  view.append(head);
  extra.forEach((e) => (e instanceof Node && e.classList && e.classList.contains('topbar-extra') ? head.append(e) : view.append(e)));
  return head;
}

function renderShell(app) {
  const s = state.session;
  const nav = h('nav', { class: 'nav', 'aria-label': 'Main' },
    PAGES.filter(([k]) => k !== 'settings' || s.authenticated).map(([k, label]) =>
      h('a', { href: '#/' + k, 'data-page': k }, icon(k), label,
        k === 'alerts' ? h('span', { class: 'count', id: 'alert-count', hidden: true, 'aria-label': 'alerts firing' }) : null)));
  const themeBtn = h('button', { class: 'ghost', type: 'button', onclick: cycleTheme }, icon('moon'), h('span', { text: 'Theme' }));
  const authBtn = s.authenticated
    ? h('button', { class: 'ghost', type: 'button', onclick: logout }, icon('out'), h('span', { text: 'Sign out' }))
    : h('a', { class: 'btn ghost', href: '#/login' }, icon('out'), h('span', { text: 'Admin sign in' }));
  const side = h('aside', { class: 'side', id: 'side' },
    h('a', { class: 'brand' + (hasBrandLogo() ? ' co' : ''), href: '#/overview' }, brandMark(''),
      h('div', {}, h('b', { text: 'vps-watcher' }), h('span', { id: 'host-name', text: '…' }))),
    nav,
    h('div', { class: 'side-foot' },
      h('div', { class: 'row' }, h('span', { class: 'live', id: 'live' }, h('i'), h('span', { text: 'Connecting…' }))),
      h('div', { class: 'row', style: { marginTop: '6px' } }, h('span', { text: 'Using' }), h('span', { id: 'own', class: 'num', text: '–' })),
      h('div', { class: 'row', style: { marginTop: '10px' } }, themeBtn, authBtn)));
  const main = h('main', { class: 'main', id: 'main' });
  const shell = h('div', { class: 'shell' }, side, main);
  fill(app, shell);
}

function cycleTheme() {
  const order = ['system', 'dark', 'light'];
  const next = order[(order.indexOf(store.get('theme', 'system')) + 1) % order.length];
  store.set('theme', next);
  applyTheme();
  toast(`Theme: ${next}`);
  window.dispatchEvent(new Event('resize'));
  route();
}

async function logout() {
  try { await api.post('/api/logout'); } catch { /* already gone */ }
  location.hash = '';
  boot();
}

/* ---------- branding ---------- */

function hasBrandLogo() {
  return Boolean(state.session && state.session.brand && state.session.brand.logo);
}

/** The vps-watcher logo, then a divider and the organization's own logo when one is set. */
function brandMark(mainAlt) {
  const b = (state.session && state.session.brand) || {};
  const main = h('img', { src: '/logo.svg', alt: mainAlt, class: 'mark-main' });
  if (!b.logo) return h('span', { class: 'mark' }, main);
  return h('span', { class: 'mark' }, main, h('span', { class: 'mark-sep', 'aria-hidden': 'true' }),
    h('img', { src: `/brand/logo?v=${encodeURIComponent(b.logo)}`, alt: b.name || 'Organization logo', title: b.name || null, class: 'mark-custom', decoding: 'async' }));
}

/** Settings card: upload or remove the organization's logo and set its name. */
function brandingCard() {
  const card = h('section', { class: 'card' });
  const MAX = 256 * 1024;
  const paint = () => {
    const b = state.session.brand || {};
    const err = h('p', { class: 'error small', role: 'alert' });
    const file = h('input', { type: 'file', accept: 'image/png,image/jpeg,image/webp,image/svg+xml' });
    const name = h('input', { maxlength: '60', placeholder: 'Acme Inc.', value: b.name || '' });
    const saveName = h('button', { type: 'button' }, 'Save name');
    const remove = b.logo ? h('button', { type: 'button', class: 'ghost danger' }, icon('trash'), 'Remove logo') : null;
    // Reload the session and redraw everything, so the sidebar shows the change at once.
    const done = async (msg) => { toast(msg); state.session = await api.get('/api/session'); await boot(); location.hash = '#/settings'; };
    file.addEventListener('change', async () => {
      const f = file.files[0];
      err.textContent = '';
      if (!f) return;
      if (f.size > MAX) { err.textContent = `That file is ${Math.ceil(f.size / 1024)} KB; the limit is 256 KB.`; return; }
      file.disabled = true;
      try {
        const r = await fetch('/api/brand/logo', { method: 'PUT', body: f, credentials: 'same-origin', headers: { 'content-type': f.type || 'application/octet-stream' } });
        const d = await r.json().catch(() => ({}));
        if (!r.ok) throw new Error(d.error || `Upload failed (${r.status})`);
        await done('Logo saved');
      } catch (e) { err.textContent = e.message; file.disabled = false; }
    });
    saveName.addEventListener('click', async () => {
      err.textContent = '';
      try { await api.post('/api/brand', { name: name.value }); await done('Name saved'); } catch (e) { err.textContent = e.message; }
    });
    if (remove) {
      remove.addEventListener('click', async () => {
        try { await api.del('/api/brand/logo'); await done('Logo removed'); } catch (e) { err.textContent = e.message; }
      });
    }
    fill(card,
      h('div', { class: 'card-head' }, h('h2', { text: 'Branding' })),
      h('div', { class: 'brand-preview' }, brandMark('vps-watcher logo')),
      h('div', { class: 'form' },
        field('Your logo', file, 'PNG, JPEG, WebP or SVG up to 256 KB. A transparent logo about 240 × 68 px (or an SVG) looks sharp in both themes.'),
        field('Organization name', name, 'Read aloud by screen readers and shown when you hover the logo.'),
        h('div', { class: 'row' }, saveName, remove), err));
  };
  paint();
  return card;
}

/* ---------- auth pages ---------- */

function authCard(title, lead, ...body) {
  return h('div', { class: 'auth' }, h('div', { class: 'card' },
    brandMark('vps-watcher logo'), h('h1', { text: title }), h('p', { class: 'lead', text: lead }), ...body));
}

let fieldSeq = 0;
/** Labeled form field; the hint is linked for screen readers. */
function field(label, input, hint) {
  const id = input.id || (input.id = 'f' + ++fieldSeq);
  if (hint) input.setAttribute('aria-describedby', id + '-hint');
  return h('div', { class: 'field' }, h('label', { for: id, text: label }), input, hint ? h('span', { class: 'hint', id: id + '-hint', text: hint }) : null);
}

function renderSetup(app) {
  document.title = 'Create admin · vps-watcher';
  if (!state.session.setup_allowed) {
    fill(app, authCard('Finish setup on the server', 'For safety, the first admin can only be created from the server itself.',
      h('p', { text: 'From your computer, open an SSH tunnel:' }),
      h('pre', { class: 'mono card', text: 'ssh -L 9100:127.0.0.1:9100 you@your-server' }),
      h('p', {}, 'Then visit ', h('b', { text: 'http://localhost:9100' }), ' and create the admin there. After that, sign in here.'),
      h('button', { onclick: boot, text: 'I created it, reload' })));
    return;
  }
  const email = h('input', { type: 'email', autocomplete: 'username', required: true, autofocus: true });
  const pw = h('input', { type: 'password', autocomplete: 'new-password', required: true, minlength: '12' });
  const pw2 = h('input', { type: 'password', autocomplete: 'new-password', required: true });
  const show = h('input', { type: 'checkbox' });
  show.addEventListener('change', () => { pw.type = pw2.type = show.checked ? 'text' : 'password'; });
  const err = h('p', { class: 'error', role: 'alert' });
  const btn = h('button', { class: 'primary', type: 'submit', text: 'Create admin' });
  const form = h('form', { class: 'form' },
    field('Email', email), field('Password', pw, 'At least 12 characters. A short sentence works well.'), field('Confirm password', pw2),
    h('label', { class: 'check small' }, show, 'Show passwords'), err, btn);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    err.textContent = '';
    if (pw.value !== pw2.value) { err.textContent = 'Passwords do not match.'; return; }
    btn.disabled = true; btn.textContent = 'Creating…';
    try {
      await api.post('/api/setup', { email: email.value, password: pw.value });
      location.hash = '#/overview';
      boot();
    } catch (x) {
      err.textContent = x.message;
      btn.disabled = false; btn.textContent = 'Create admin';
    }
  });
  fill(app, authCard('Welcome to vps-watcher', 'Create the admin account. You will use it to sign in.', form));
}

function renderLogin(app) {
  document.title = 'Sign in · vps-watcher';
  const email = h('input', { type: 'email', autocomplete: 'username', required: true, autofocus: true });
  const pw = h('input', { type: 'password', autocomplete: 'current-password', required: true });
  const err = h('p', { class: 'error', role: 'alert' });
  const btn = h('button', { class: 'primary', type: 'submit', text: 'Sign in' });
  const form = h('form', { class: 'form' }, field('Email', email), field('Password', pw), err, btn);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    err.textContent = '';
    btn.disabled = true; btn.textContent = 'Signing in…';
    try {
      await api.post('/api/login', { email: email.value, password: pw.value });
      if (location.hash === '#/login') location.hash = '#/overview';
      boot();
    } catch (x) {
      err.textContent = x.message;
      btn.disabled = false; btn.textContent = 'Sign in';
      pw.select();
    }
  });
  const back = state.session.mode === 'open' ? h('p', { class: 'small', style: { textAlign: 'center', marginTop: '14px' } }, h('a', { href: '#/overview', text: 'Back to dashboard' })) : null;
  fill(app, authCard('Sign in', state.session.mode === 'open' ? 'Sign in as admin to manage monitors and settings.' : 'This server is monitored by vps-watcher.', form, back));
}

/* ---------- Overview ---------- */

function pageOverview(view) {
  const rangeEl = rangePicker(state.range, (r) => { state.range = r; store.set('range', r); loadAll(); });
  pageTitle(view, 'Overview');
  const banner = h('div', { class: 'banner', hidden: true });
  const strip = h('a', { class: 'alert-strip', href: '#/alerts', hidden: true });
  const up = state.session.update && state.session.update.status;
  const updateNote = up && up.available
    ? h('a', { class: 'update-strip', href: '#/settings' }, icon('download'),
      h('b', { text: `vps-watcher ${up.latest} is available` }),
      h('span', { class: 'grow', text: state.session.update.auto ? 'It installs automatically within a day.' : `You run ${up.current}. Upgrade with: sudo vps-watcher upgrade` }),
      h('span', { text: 'Details →' }))
    : null;
  const kpis = h('div', { class: 'grid kpis' });
  const cpu = chartCard('CPU', { unit: '%', max: 100, series: [{ label: 'CPU', color: '--s1' }] });
  const mem = chartCard('Memory', { unit: '%', max: 100, series: [{ label: 'Memory', color: '--s1' }] });
  const net = chartCard('Network', { unit: 'B/s', series: [{ label: 'In', color: '--s1' }, { label: 'Out', color: '--s2' }] });
  const io = chartCard('Disk I/O', { unit: 'B/s', series: [{ label: 'Read', color: '--s1' }, { label: 'Write', color: '--s2' }] });
  const appsCard = h('section', { class: 'card' });
  const sideCard = h('div', { class: 'stack' });
  view.append(banner, strip, updateNote || '', kpis,
    h('div', { class: 'toolbar' }, h('h2', { text: 'History' }), rangeEl),
    h('div', { class: 'grid charts' }, cpu.card, mem.card, net.card, io.card),
    h('div', { class: 'grid two' }, appsCard, sideCard));

  const sparks = {};
  function loadAll() {
    loadCharts([
      { chart: cpu.chart, keys: ['host.cpu'] },
      { chart: mem.chart, keys: ['host.mem'] },
      { chart: net.chart, keys: ['host.net.rx', 'host.net.tx'] },
      { chart: io.chart, keys: ['host.disk.read', 'host.disk.write'] },
    ], state.range);
  }
  async function loadSparks() {
    try {
      const d = await api.get('/api/history?range=1h&keys=host.cpu,host.mem,host.net.rx,host.load1');
      for (const k in d.series) sparks[k] = d.series[k].values.slice(-120);
      paint(state.snap);
    } catch { /* tiles still show live values */ }
  }

  function kpi(label, value, unit, sub, sparkKey, max, extra) {
    const c = sparkKey ? h('canvas', { 'aria-hidden': 'true' }) : null;
    const el = h('section', { class: 'card kpi' },
      h('span', { class: 'label', text: label }),
      h('span', { class: 'value' }, value, unit ? h('small', { text: unit }) : null),
      h('span', { class: 'sub', text: sub }), extra || null, c);
    if (c && sparks[sparkKey]) requestAnimationFrame(() => sparkline(c, sparks[sparkKey], '--s1', max));
    return el;
  }

  function paint(s) {
    if (!s) return;
    const notes = [];
    if (s.docker.error) notes.push(`Docker is not reachable: ${s.docker.error}`);
    if (state.session.mode === 'open' && !state.session.authenticated) notes.push('Public view (open mode). Sign in as admin to manage monitors.');
    banner.hidden = !notes.length;
    banner.textContent = notes.join(' · ');
    const firing = s.alerts || [];
    strip.hidden = !firing.length;
    strip.classList.toggle('warn', firing.every((a) => a.severity !== 'critical'));
    fill(strip, icon('alerts'), h('b', { text: `${firing.length} alert${firing.length === 1 ? '' : 's'} firing` }),
      h('span', { class: 'grow', text: firing.slice(0, 3).map((a) => a.title).join(' · ') + (firing.length > 3 ? ' …' : '') }),
      h('span', { text: 'View →' }));

    const disk = [...s.disks].sort((a, b) => b.pct - a.pct)[0];
    const diskPct = disk ? disk.pct : 0;
    const memPct = s.mem.total ? (s.mem.used * 100) / s.mem.total : 0;
    const [pv, pu] = splitPct(s.cpu.total);
    fill(kpis, 
      kpi('CPU', pv, pu, `steal ${fmt.pct(s.cpu.steal)} · I/O wait ${fmt.pct(s.cpu.iowait)} · ${s.cores} cores`, 'host.cpu', 100),
      kpi('Memory', ...splitPct(memPct), `${fmt.bytes(s.mem.used)} of ${fmt.bytes(s.mem.total)} · ${fmt.bytes(s.mem.available)} free`, 'host.mem', 100),
      kpi('Disk', ...splitPct(diskPct), disk ? `${disk.mount} · ${fmt.bytes(disk.avail)} free of ${fmt.bytes(disk.total)}` : 'no disks found', null, null, meter(diskPct)),
      kpi('Network in', fmt.rate(s.net.a), '', `out ${fmt.rate(s.net.b)} · ${s.ifaces.length} interface${s.ifaces.length === 1 ? '' : 's'}`, 'host.net.rx'),
      kpi('Load', fmt.num(s.load[0], 2), '', `5m ${fmt.num(s.load[1], 2)} · 15m ${fmt.num(s.load[2], 2)} · ${s.cores} cores`, 'host.load1'),
      kpi('Uptime', fmt.dur(s.uptime), '', `${s.os} · kernel ${s.kernel.split('-')[0]}`),
    );

    const apps = s.apps.filter((a) => !a.system && a.state === 'running').sort((a, b) => (b.cpu_pct || 0) - (a.cpu_pct || 0)).slice(0, 8);
    const problems = s.apps.filter((a) => a.state === 'failed' || a.health === 'unhealthy' || a.state === 'restarting' || a.state === 'exited');
    const t = sortableTable([
      { key: 'name', label: 'App', render: (a) => nameCell(a.name, a.kind === 'container' ? a.image : 'systemd service') },
      { key: 'state', label: 'Status', render: stateBadge },
      { key: 'cpu', label: 'CPU', num: true, render: (a) => fmt.pct(a.cpu_pct) },
      { key: 'mem', label: 'Memory', num: true, render: (a) => fmt.bytes(a.mem) },
    ], { onRow: (a) => (location.hash = '#/app/' + encodeURIComponent(a.id)), empty: 'No containers or services found yet.' });
    t.set(apps);
    fill(appsCard, 
      h('div', { class: 'card-head' }, h('h2', { text: 'Top apps by CPU' }), h('a', { href: '#/apps', text: `All ${s.apps.length} apps →` })),
      problems.length ? h('p', { class: 'small' }, h('span', { class: 'badge bad', text: `${problems.length} need attention` }), ' ', problems.slice(0, 4).map((p) => p.name).join(', ')) : null,
      t.el);

    const mons = h('section', { class: 'card' },
      h('div', { class: 'card-head' }, h('h2', { text: 'Monitors' }), h('a', { href: '#/monitors', text: 'Manage →' })),
      s.monitors.length
        ? h('div', { class: 'stack' }, s.monitors.slice(0, 6).map((m) => h('div', { class: 'row' }, upBadge(m.up), h('b', { text: m.name }), h('span', { class: 'spacer' }), h('span', { class: 'muted small', text: hasLatency(m) ? `${Math.round(m.ms)} ms` : m.detail }))))
        : h('p', { class: 'muted small', text: 'No monitors yet. Add a URL, port, process or log file to watch.' }));
    const disks = h('section', { class: 'card' }, h('div', { class: 'card-head' }, h('h2', { text: 'Disks' })),
      h('div', { class: 'stack' }, s.disks.map((d) => {
        const p = d.pct;
        return h('div', {}, h('div', { class: 'row' }, h('b', { text: d.mount }), h('span', { class: 'muted small', text: d.fstype }), h('span', { class: 'spacer' }),
          h('span', { class: 'num small', text: `${fmt.bytes(d.used)} / ${fmt.bytes(d.total)} · ${fmt.pct(p)}` })), meter(p),
          d.inodes_pct > 80 ? h('p', { class: 'small', style: { color: 'var(--warn)' }, text: `Inodes ${fmt.pct(d.inodes_pct)} used` }) : null);
      })));
    fill(sideCard, mons, disks);
  }

  paint(state.snap);
  state.onSnap = paint;
  loadAll();
  loadSparks();
  const iv = setInterval(() => { if (!document.hidden) { loadSparks(); if (state.range === '1h') loadAll(); } }, 30000);
  state.cleanup.push(() => clearInterval(iv));
}

/** Response time is meaningful only for network checks. */
function hasLatency(m) {
  return (m.kind === 'http' || m.kind === 'tcp') && m.ms !== null && m.ms !== undefined && m.up;
}

/** Splits "42.5%" into big number and small unit for tiles. */
function splitPct(n) {
  const s = fmt.pct(n);
  return s.endsWith('%') ? [s.slice(0, -1), '%'] : [s, ''];
}

/* ---------- Apps ---------- */

function pageApps(view) {
  pageTitle(view, 'Apps');
  const search = h('input', { type: 'search', placeholder: 'Filter by name or image', 'aria-label': 'Filter apps', class: 'grow' });
  let kind = store.get('apps.kind', 'all');
  const kinds = h('div', { class: 'seg', role: 'group', 'aria-label': 'Kind' }, ['all', 'container', 'service'].map((k) => {
    const b = h('button', { type: 'button', 'aria-pressed': String(k === kind), text: k === 'all' ? 'All' : k === 'container' ? 'Containers' : 'Services' });
    b.addEventListener('click', () => { kind = k; store.set('apps.kind', k); kinds.querySelectorAll('button').forEach((x) => x.setAttribute('aria-pressed', String(x === b))); paint(state.snap); });
    return b;
  }));
  const sys = h('input', { type: 'checkbox' });
  sys.checked = store.get('apps.system', '0') === '1';
  sys.addEventListener('change', () => { store.set('apps.system', sys.checked ? '1' : '0'); paint(state.snap); });
  search.addEventListener('input', () => paint(state.snap));
  const count = h('span', { class: 'muted small' });
  const t = sortableTable([
    { key: 'name', label: 'Name', get: (a) => a.name, render: (a) => nameCell(a.name, a.kind === 'container' ? [a.image, a.group && `project ${a.group}`].filter(Boolean).join(' · ') : 'systemd service') },
    { key: 'kind', label: 'Kind', get: (a) => a.kind, render: (a) => h('span', { class: 'tag', text: a.kind }) },
    { key: 'state', label: 'Status', get: (a) => a.state, render: (a) => h('div', { class: 'row' }, stateBadge(a), a.oom_killed ? h('span', { class: 'badge bad plain', text: 'OOM killed', title: 'Last exit was the out-of-memory killer' }) : null) },
    { key: 'cpu', label: 'CPU', num: true, get: (a) => a.cpu_pct, render: (a) => fmt.pct(a.cpu_pct) },
    { key: 'mem', label: 'Memory', num: true, get: (a) => a.mem, render: (a) => fmt.bytes(a.mem) + (a.mem_limit ? ` / ${fmt.bytes(a.mem_limit)}` : '') },
    { key: 'net', label: 'Net in / out', num: true, get: (a) => a.net && a.net.a + a.net.b, render: (a) => (a.net ? `${fmt.rate(a.net.a)} / ${fmt.rate(a.net.b)}` : '–') },
    { key: 'io', label: 'Disk r / w', num: true, get: (a) => a.io && a.io.a + a.io.b, render: (a) => (a.io ? `${fmt.rate(a.io.a)} / ${fmt.rate(a.io.b)}` : '–') },
    { key: 'restarts', label: 'Restarts', num: true, get: (a) => a.restarts, render: (a) => (a.restarts === null || a.restarts === undefined ? '–' : String(a.restarts)) },
  ], { id: 'apps', defaultSort: 'cpu:desc', onRow: (a) => (location.hash = '#/app/' + encodeURIComponent(a.id)), empty: 'Nothing matches.' });
  view.append(h('div', { class: 'toolbar' }, search, kinds, h('label', { class: 'check' }, sys, 'Show system services'), count), h('section', { class: 'card' }, t.el));

  function paint(s) {
    if (!s) return;
    const q = search.value.trim().toLowerCase();
    const rows = s.apps.filter((a) => (kind === 'all' || a.kind === kind) && (sys.checked || !a.system)
      && (!q || a.name.toLowerCase().includes(q) || (a.image || '').toLowerCase().includes(q) || (a.group || '').toLowerCase().includes(q)));
    count.textContent = `${rows.length} of ${s.apps.length}`;
    t.set(rows);
  }
  paint(state.snap);
  state.onSnap = paint;
}

/* ---------- App detail ---------- */

function pageApp(view, id) {
  const app0 = state.snap && state.snap.apps.find((a) => a.id === id);
  pageTitle(view, app0 ? app0.name : 'App');
  if (!app0) {
    view.append(h('section', { class: 'card empty-state' }, h('p', { text: 'This app is not running or no longer exists.' }), h('a', { href: '#/apps', text: '← Back to apps' })));
    return;
  }
  const info = h('dl', { class: 'kv' });
  const head = h('div', { class: 'row' });
  const range = rangePicker(state.range, (r) => { state.range = r; store.set('range', r); load(); });
  const cpu = chartCard('CPU (% of host)', { unit: '%', series: [{ label: 'CPU', color: '--s1' }] });
  const mem = chartCard('Memory', { unit: 'B', series: [{ label: 'Memory', color: '--s1' }] });
  const net = chartCard('Network', { unit: 'B/s', series: [{ label: 'In', color: '--s1' }, { label: 'Out', color: '--s2' }] });
  const io = chartCard('Disk I/O', { unit: 'B/s', series: [{ label: 'Read', color: '--s1' }, { label: 'Write', color: '--s2' }] });
  const logBox = h('div', { class: 'logview', style: { height: '320px' } });
  const canLogs = state.session.authenticated || state.session.open_logs;
  view.append(
    h('div', { class: 'toolbar' }, h('a', { class: 'btn', href: '#/apps' }, icon('back'), 'Apps'), head, h('span', { class: 'spacer' }),
      canLogs ? h('a', { class: 'btn', href: '#/logs?src=' + encodeURIComponent('app:' + id) }, icon('logs'), 'Open logs') : null),
    h('section', { class: 'card', style: { marginBottom: '14px' } }, info),
    h('div', { class: 'toolbar' }, h('h2', { text: 'History' }), range),
    h('div', { class: 'grid charts' }, cpu.card, mem.card, net.card, io.card),
    canLogs ? h('section', { class: 'card' }, h('div', { class: 'card-head' }, h('h2', { text: 'Recent logs' })), logBox) : null);

  function paint(s) {
    const a = s && s.apps.find((x) => x.id === id);
    if (!a) return;
    fill(head, stateBadge(a), h('span', { class: 'tag', text: a.kind }), a.oom_killed ? h('span', { class: 'badge bad plain', text: 'last exit: out of memory' }) : null);
    const rows = [
      ['Status', a.status],
      a.image && ['Image', a.image],
      a.group && ['Compose project', a.group],
      ['CPU', fmt.pct(a.cpu_pct)],
      ['Memory', fmt.bytes(a.mem) + (a.mem_limit ? ` of ${fmt.bytes(a.mem_limit)} limit` : ' (no limit)')],
      a.pids !== null && a.pids !== undefined && ['Processes', String(a.pids)],
      a.restarts !== null && a.restarts !== undefined && ['Restarts', String(a.restarts)],
      a.health && ['Health check', a.health],
      a.ports && a.ports.length && ['Ports', a.ports.map((p) => (p.public_port ? `${p.ip || '0.0.0.0'}:${p.public_port} → ${p.private_port}/${p.proto}` : `${p.private_port}/${p.proto}`)).join(', ')],
    ].filter(Boolean);
    fill(info, ...rows.flatMap(([k, v]) => [h('dt', { text: k }), h('dd', { text: v })]));
  }
  function load() {
    const p = 'app:' + id;
    loadCharts([
      { chart: cpu.chart, keys: [p + '.cpu'] },
      { chart: mem.chart, keys: [p + '.mem'] },
      { chart: net.chart, keys: [p + '.net.rx', p + '.net.tx'] },
      { chart: io.chart, keys: [p + '.io.r', p + '.io.w'] },
    ], state.range);
  }
  async function loadLogs() {
    if (!canLogs) return;
    try {
      const d = await api.get(`/api/logs?source=${encodeURIComponent('app:' + id)}&lines=60`);
      renderLogLines(logBox, d.lines, '');
      logBox.scrollTop = logBox.scrollHeight;
    } catch (e) {
      fill(logBox, h('div', { class: 'empty', text: e.message }));
    }
  }
  paint(state.snap);
  state.onSnap = paint;
  load();
  loadLogs();
  const iv = setInterval(() => { if (!document.hidden && state.range === '1h') load(); }, 30000);
  state.cleanup.push(() => clearInterval(iv));
}

/* ---------- Logs ---------- */

const ERR_RE = /\b(error|err|fatal|panic|critical|crit|exception|failed|failure)\b/i;
const WARN_RE = /\b(warn|warning|deprecated|timeout|retry)\b/i;

/** Renders log lines with timestamps, level colors and safe match highlighting. */
function renderLogLines(box, lines, q) {
  if (!lines.length) {
    fill(box, h('div', { class: 'empty', text: q ? 'No lines match.' : 'No log lines yet.' }));
    return;
  }
  const needle = q.toLowerCase();
  fill(box, ...lines.map((l) => {
    const cls = l.stream === 'stderr' || ERR_RE.test(l.text) ? 'err' : WARN_RE.test(l.text) ? 'wrn' : '';
    const text = h('span', { class: cls });
    if (needle) {
      let rest = l.text, i;
      while ((i = rest.toLowerCase().indexOf(needle)) >= 0) {
        text.append(rest.slice(0, i), h('mark', { text: rest.slice(i, i + needle.length) }));
        rest = rest.slice(i + needle.length);
      }
      text.append(rest);
    } else text.textContent = l.text;
    return h('div', { class: 'ln' }, l.ts ? h('span', { class: 'ts', text: fmt.stamp(l.ts) }) : null, text);
  }));
}

async function pageLogs(view, q) {
  pageTitle(view, 'Logs');
  const sel = h('select', { 'aria-label': 'Log source' });
  const search = h('input', { type: 'search', placeholder: 'Search text', 'aria-label': 'Search logs', class: 'grow' });
  const lines = h('select', { 'aria-label': 'Lines' }, [100, 200, 500, 1000].map((n) => h('option', { value: String(n), text: `${n} lines` })));
  lines.value = store.get('logs.lines', '200');
  const errOnly = h('input', { type: 'checkbox' });
  const live = h('input', { type: 'checkbox' });
  live.checked = store.get('logs.live', '1') === '1';
  const status = h('span', { class: 'muted small' });
  const box = h('div', { class: 'logview', tabindex: '0', 'aria-label': 'Log output' });
  view.append(h('div', { class: 'toolbar' }, sel, search, lines,
    h('label', { class: 'check' }, errOnly, 'Errors & warnings'),
    h('label', { class: 'check' }, live, 'Live'), status), box);

  let sources;
  try {
    sources = await api.get('/api/log-sources');
  } catch (e) {
    fill(box, h('div', { class: 'empty', text: e.message }));
    return;
  }
  const group = (label, items) => (items.length ? h('optgroup', { label }, items.map(([v, t]) => h('option', { value: v, text: t }))) : null);
  const apps = sources.apps;
  sel.append(
    group('Containers', apps.filter((a) => a.kind === 'container').map((a) => ['app:' + a.id, `${a.name}${a.state !== 'running' ? ` (${a.state})` : ''}`])),
    group('Services', apps.filter((a) => a.kind === 'service' && !a.system).map((a) => ['app:' + a.id, a.name])),
    group('Log files', sources.files.map((f) => ['file:' + f, f])),
    group('System services', apps.filter((a) => a.kind === 'service' && a.system).map((a) => ['app:' + a.id, a.name])));
  if (!sel.options.length) {
    fill(box, h('div', { class: 'empty', text: 'No log sources found.' }));
    return;
  }
  const want = q.get('src') || store.get('logs.src', '');
  if ([...sel.options].some((o) => o.value === want)) sel.value = want;

  let busy = false;
  let timer = null;
  async function load() {
    if (busy) return;
    busy = true;
    const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
    try {
      const d = await api.get(`/api/logs?source=${encodeURIComponent(sel.value)}&lines=${lines.value}&q=${encodeURIComponent(search.value.trim())}`);
      const shown = errOnly.checked ? d.lines.filter((l) => l.stream === 'stderr' || ERR_RE.test(l.text) || WARN_RE.test(l.text)) : d.lines;
      renderLogLines(box, shown, search.value.trim());
      status.textContent = `${shown.length} lines · ${fmt.time(d.now)}`;
      if (atBottom || !box.dataset.loaded) box.scrollTop = box.scrollHeight;
      box.dataset.loaded = '1';
    } catch (e) {
      fill(box, h('div', { class: 'empty', text: e.message }));
      status.textContent = '';
    }
    busy = false;
  }
  function schedule() {
    clearInterval(timer);
    if (live.checked) timer = setInterval(() => { if (!document.hidden) load(); }, 3000);
  }
  let debounce;
  search.addEventListener('input', () => { clearTimeout(debounce); debounce = setTimeout(load, 300); });
  sel.addEventListener('change', () => { store.set('logs.src', sel.value); delete box.dataset.loaded; load(); });
  lines.addEventListener('change', () => { store.set('logs.lines', lines.value); load(); });
  errOnly.addEventListener('change', load);
  live.addEventListener('change', () => { store.set('logs.live', live.checked ? '1' : '0'); schedule(); });
  state.cleanup.push(() => clearInterval(timer));
  load();
  schedule();
}

/* ---------- System ---------- */

function pageSystem(view, q) {
  pageTitle(view, 'System');
  const tabs = [['processes', 'Processes'], ['ports', 'Ports'], ['disks', 'Disks'], ['network', 'Network']];
  let tab = q.get('tab') || store.get('system.tab', 'processes');
  const canProcs = state.session.authenticated || state.session.open_logs;
  if (tab === 'processes' && !canProcs) tab = 'ports';
  const seg = h('div', { class: 'seg', role: 'tablist' });
  const body = h('div');
  for (const [k, label] of tabs) {
    if (k === 'processes' && !canProcs) continue;
    const b = h('button', { type: 'button', role: 'tab', 'aria-pressed': String(k === tab), text: label });
    b.addEventListener('click', () => { tab = k; store.set('system.tab', k); seg.querySelectorAll('button').forEach((x) => x.setAttribute('aria-pressed', String(x === b))); show(); });
    seg.append(b);
  }
  view.append(h('div', { class: 'toolbar' }, seg), body);
  let timer = null;
  state.cleanup.push(() => clearInterval(timer));

  function show() {
    clearInterval(timer);
    state.onSnap = null;
    fill(body);
    ({ processes: showProcs, ports: showPorts, disks: showDisks, network: showNet })[tab]();
  }

  function showProcs() {
    const filter = h('input', { type: 'search', placeholder: 'Filter by name, command or user', class: 'grow', 'aria-label': 'Filter processes' });
    const info = h('span', { class: 'muted small' });
    const refresh = h('button', { type: 'button', onclick: () => load() }, icon('refresh'), 'Refresh');
    const t = sortableTable([
      { key: 'pid', label: 'PID', num: true, get: (p) => p.pid },
      { key: 'name', label: 'Process', get: (p) => p.name, render: (p) => nameCell(p.name, p.cmd) },
      { key: 'user', label: 'User', get: (p) => p.user },
      { key: 'cpu', label: 'CPU', num: true, get: (p) => p.cpu_pct, render: (p) => fmt.pct(p.cpu_pct) },
      { key: 'mem', label: 'Memory', num: true, get: (p) => p.mem, render: (p) => fmt.bytes(p.mem) },
      { key: 'threads', label: 'Threads', num: true, get: (p) => p.threads },
      { key: 'state', label: 'State', get: (p) => p.state },
    ], { id: 'procs', defaultSort: 'cpu:desc', empty: 'No processes match.' });
    let rows = [];
    const paint = () => {
      const f = filter.value.trim().toLowerCase();
      t.set(f ? rows.filter((p) => (p.name + ' ' + p.cmd + ' ' + p.user).toLowerCase().includes(f)) : rows);
    };
    filter.addEventListener('input', paint);
    async function load() {
      refresh.disabled = true;
      try {
        const d = await api.get('/api/processes');
        rows = d.processes;
        info.textContent = `top ${rows.length} of ${d.total} · ${fmt.time(Date.now() / 1000)}`;
        paint();
      } catch (e) { info.textContent = e.message; }
      refresh.disabled = false;
    }
    body.append(h('div', { class: 'toolbar' }, filter, refresh, info), h('section', { class: 'card' }, t.el));
    load();
    timer = setInterval(() => { if (!document.hidden) load(); }, 10000);
  }

  async function showPorts() {
    const card = h('section', { class: 'card' }, h('p', { class: 'muted', text: 'Scanning…' }));
    body.append(h('p', { class: 'muted small', text: 'Ports bound to all interfaces are reachable from the internet unless a firewall blocks them.' }), card);
    try {
      const d = await api.get('/api/ports');
      const t = sortableTable([
        { key: 'port', label: 'Port', num: true, get: (p) => p.port },
        { key: 'proto', label: 'Proto', get: (p) => p.proto, render: (p) => h('span', { class: 'tag', text: p.proto }) },
        { key: 'addr', label: 'Address', get: (p) => p.addr, render: (p) => h('div', { class: 'row' }, h('span', { class: 'mono', text: p.addr }), p.public ? h('span', { class: 'badge warn plain', text: 'all interfaces' }) : null) },
        { key: 'process', label: 'Process', get: (p) => p.process || '', render: (p) => (p.process ? `${p.process} (${p.pid})` : 'unknown') },
        { key: 'app', label: 'App', get: (p) => p.app || '', render: (p) => p.app || '–' },
      ], { id: 'ports', defaultSort: 'port:asc', empty: 'No listening ports.' });
      t.set(d.ports);
      fill(card, t.el);
    } catch (e) { fill(card, h('p', { class: 'error', text: e.message })); }
  }

  function showDisks() {
    const t = sortableTable([
      { key: 'mount', label: 'Mount', get: (d) => d.mount, render: (d) => nameCell(d.mount, `${d.device} · ${d.fstype}`) },
      { key: 'used', label: 'Used', num: true, get: (d) => d.pct, render: (d) => h('div', {}, `${fmt.bytes(d.used)} of ${fmt.bytes(d.total)} · ${fmt.pct(d.pct)}`, meter(d.pct)) },
      { key: 'avail', label: 'Free', num: true, get: (d) => d.avail, render: (d) => fmt.bytes(d.avail) },
      { key: 'inodes', label: 'Inodes used', num: true, get: (d) => d.inodes_pct, render: (d) => fmt.pct(d.inodes_pct) },
    ], { id: 'disks', defaultSort: 'used:desc', empty: 'No disks found.' });
    body.append(h('section', { class: 'card' }, t.el));
    const paint = (s) => s && t.set(s.disks);
    paint(state.snap);
    state.onSnap = paint;
  }

  function showNet() {
    const t = sortableTable([
      { key: 'name', label: 'Interface', get: (i) => i.name },
      { key: 'rx', label: 'In', num: true, get: (i) => i.rx_bps, render: (i) => fmt.rate(i.rx_bps) },
      { key: 'tx', label: 'Out', num: true, get: (i) => i.tx_bps, render: (i) => fmt.rate(i.tx_bps) },
      { key: 'rxt', label: 'Received total', num: true, get: (i) => i.rx_total, render: (i) => fmt.bytes(i.rx_total) },
      { key: 'txt', label: 'Sent total', num: true, get: (i) => i.tx_total, render: (i) => fmt.bytes(i.tx_total) },
    ], { id: 'net', defaultSort: 'rx:desc', empty: 'No network interfaces found.' });
    body.append(h('section', { class: 'card' }, t.el));
    const paint = (s) => s && t.set(s.ifaces);
    paint(state.snap);
    state.onSnap = paint;
  }

  show();
}

/* ---------- Monitors ---------- */

async function pageMonitors(view) {
  pageTitle(view, 'Monitors');
  const admin = state.session.authenticated;
  const listCard = h('section', { class: 'card' });
  view.append(h('p', { class: 'muted', text: 'Watch things vps-watcher cannot find on its own: a website or API, a port, a background process, or a log file. Checks run every 30 seconds; processes every sample.' }), listCard);
  let avail = {};
  const t = sortableTable([
    { key: 'name', label: 'Name', get: (m) => m.name, render: (m) => nameCell(m.name, m.target) },
    { key: 'kind', label: 'Type', get: (m) => m.kind, render: (m) => h('span', { class: 'tag', text: m.kind }) },
    { key: 'up', label: 'Status', get: (m) => (m.up === true ? 1 : m.up === false ? 0 : -1), render: (m) => upBadge(m.up) },
    { key: 'detail', label: 'Detail', render: (m) => (m.kind === 'process' ? `${m.detail} · ${fmt.pct(m.cpu_pct)} CPU · ${fmt.bytes(m.mem)}` : hasLatency(m) ? `${m.detail} · ${Math.round(m.ms)} ms` : m.detail) },
    { key: 'avail', label: 'Up (24h)', num: true, get: (m) => avail[m.id], render: (m) => fmt.pct(avail[m.id]) },
    admin ? { key: 'x', label: '', render: (m) => h('button', { class: 'ghost icon-btn danger', 'aria-label': `Delete ${m.name}`, title: 'Delete', onclick: (e) => { e.stopPropagation(); remove(m); } }, icon('trash')) } : null,
  ].filter(Boolean), { id: 'mons', defaultSort: 'name:asc', empty: 'No monitors yet.' });
  listCard.append(t.el);

  async function remove(m) {
    if (!window.confirm(`Delete monitor "${m.name}"? Its history stays until it expires.`)) return;
    try { await api.del('/api/monitors/' + encodeURIComponent(m.id)); toast('Monitor deleted'); refreshSnap(); }
    catch (e) { toast(e.message); }
  }
  async function loadAvail() {
    try {
      const r = await api.get('/api/report?range=24h');
      avail = Object.fromEntries(r.monitors.map((m) => [m.id, m.availability]));
      paint(state.snap);
    } catch { /* optional column */ }
  }
  const paint = (s) => s && t.set(s.monitors);
  paint(state.snap);
  state.onSnap = paint;
  loadAvail();
  if (admin) view.append(addMonitorForm());
}

function addMonitorForm() {
  const name = h('input', { required: true, maxlength: '60', placeholder: 'My API' });
  const type = h('select', {}, [['http', 'Website / HTTP(S) URL'], ['tcp', 'TCP port'], ['process', 'Process'], ['logfile', 'Log file']].map(([v, t]) => h('option', { value: v, text: t })));
  const url = h('input', { type: 'url', placeholder: 'https://example.com/health' });
  const expect = h('input', { type: 'number', min: '100', max: '599', placeholder: 'any below 400' });
  const host = h('input', { placeholder: '127.0.0.1' });
  const port = h('input', { type: 'number', min: '1', max: '65535', placeholder: '5432' });
  const pattern = h('input', { placeholder: 'node server.js' });
  const path = h('input', { placeholder: '/var/log/myapp/app.log' });
  const groups = {
    http: [field('URL', url), field('Expected status', expect, 'Optional')],
    tcp: [field('Host', host), field('Port', port)],
    process: [field('Name or command contains', pattern, 'Matches process names and full command lines, case-insensitive.')],
    logfile: [field('Absolute path', path, 'The file also becomes available on the Logs page.')],
  };
  const dyn = h('div', { class: 'grid two', style: { marginBottom: '0' } });
  const err = h('p', { class: 'error', role: 'alert' });
  const btn = h('button', { class: 'primary', type: 'submit', text: 'Add monitor' });
  const suggest = h('div', { class: 'row small' });
  const sync = () => fill(dyn, ...groups[type.value]);
  type.addEventListener('change', sync);
  sync();
  const form = h('form', { class: 'form' },
    h('div', { class: 'grid two', style: { marginBottom: '0' } }, field('Name', name), field('Type', type)), dyn, suggest, err,
    h('div', {}, btn));
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    err.textContent = '';
    const body = { name: name.value, type: type.value };
    if (type.value === 'http') { body.url = url.value.trim(); if (expect.value) body.expect_status = Number(expect.value); }
    if (type.value === 'tcp') { body.host = host.value.trim() || '127.0.0.1'; body.port = Number(port.value); }
    if (type.value === 'process') body.pattern = pattern.value.trim();
    if (type.value === 'logfile') body.path = path.value.trim();
    btn.disabled = true;
    try {
      await api.post('/api/monitors', body);
      toast('Monitor added. First check within 30 seconds.');
      form.reset();
      sync();
      refreshSnap();
    } catch (x) { err.textContent = x.message; }
    btn.disabled = false;
  });
  // Offer detected listening ports as one-click TCP monitors.
  api.get('/api/ports').then((d) => {
    const seen = new Set();
    const ps = d.ports.filter((p) => p.proto === 'tcp' && p.port !== 9100 && !seen.has(p.port) && seen.add(p.port)).slice(0, 12);
    if (!ps.length) return;
    suggest.append(h('span', { class: 'muted', text: 'Detected ports:' }), ...ps.map((p) => h('button', {
      type: 'button', class: 'chip', text: `${p.app || p.process || 'port'} :${p.port}`,
      onclick: () => { type.value = 'tcp'; sync(); name.value = `${p.app || p.process || 'Port'} ${p.port}`; host.value = p.addr === '0.0.0.0' || p.addr === '::' ? '127.0.0.1' : p.addr; port.value = String(p.port); },
    })));
  }).catch(() => {});
  return h('section', { class: 'card', style: { marginTop: '14px' } }, h('div', { class: 'card-head' }, h('h2', { text: 'Add a monitor' })), form);
}

/* ---------- Alerts ---------- */

function sevBadge(sev) {
  return h('span', { class: 'badge ' + (sev === 'critical' ? 'bad' : sev === 'warning' ? 'warn' : ''), text: sev });
}

function stateTag(st) {
  return h('span', { class: 'badge ' + (st === 'firing' ? 'bad' : st === 'resolved' ? 'good' : 'warn'), text: st === 'firing' ? 'alert' : st });
}

async function pageAlerts(view) {
  pageTitle(view, 'Alerts');
  const admin = state.session.authenticated;
  const activeCard = h('section', { class: 'card' });
  const histCard = h('section', { class: 'card' });
  const side = h('div', { class: 'stack' });
  view.append(h('p', { class: 'muted', text: 'vps-watcher checks every sample against the rules below. Problems must last a little while before they alert, so short blips stay quiet.' }),
    h('div', { class: 'grid two' }, h('div', { class: 'stack' }, activeCard, histCard), side));

  const hist = sortableTable([
    { key: 'ts', label: 'When', num: true, get: (e) => e.ts, render: (e) => h('span', { title: fmt.stamp(e.ts), text: fmt.time(e.ts, true) }) },
    { key: 'state', label: 'Event', get: (e) => e.state, render: (e) => stateTag(e.state) },
    { key: 'title', label: 'What', get: (e) => e.title, render: (e) => h('div', { class: 'name-cell wrap' }, h('b', { text: e.title }), e.detail ? h('span', { text: e.detail }) : null) },
  ], { id: 'ahist', defaultSort: 'ts:desc', empty: 'No alerts yet. That is good news.' });
  fill(histCard, h('div', { class: 'card-head' }, h('h2', { text: 'History' }), h('span', { class: 'muted small', text: 'last 200 events' })), hist.el);

  function paintActive(list) {
    fill(activeCard, h('div', { class: 'card-head' }, h('h2', { text: 'Firing now' })),
      list.length
        ? h('div', { class: 'stack' }, list.map((a) => h('div', { class: 'alert-row' }, sevBadge(a.severity),
          h('div', { class: 'grow' }, h('b', { text: a.title }), a.detail ? h('div', { class: 'muted small', text: a.detail }) : null),
          h('span', { class: 'muted small nowrap', title: fmt.stamp(a.since), text: 'for ' + fmt.dur(Math.max(0, Math.round(Date.now() / 1000 - a.since))) }))))
        : h('p', { class: 'row' }, h('span', { class: 'badge good', text: 'all clear' }), h('span', { class: 'muted', text: 'Nothing is firing.' })));
  }

  function rulesCard(r) {
    const pct = (v) => (v === null || v === undefined ? 'off' : `${v}% or more`);
    const rows = [
      ['CPU', r.cpu == null ? 'off' : `${pct(r.cpu)} for ${r.after_mins} min`],
      ['Memory', r.memory == null ? 'off' : `${pct(r.memory)} for ${r.after_mins} min`],
      ['Spikes', r.spike == null ? 'off' : `CPU or memory at ${r.spike}% for 1 min (critical)`],
      ['Load', r.load_per_core == null ? 'off' : `above ${r.load_per_core} per core for ${r.after_mins} min`],
      ['Disk space and inodes', r.disk == null ? 'off' : `${pct(r.disk)} used`],
      ['Monitors', 'down for 2 checks in a row'],
      ['Apps', r.apps ? 'stopped, unhealthy, restarted, out of memory, failed services, Docker unreachable' : 'off'],
      ['Emailed report', r.report_days == null ? 'off' : r.report_days === 1 ? 'daily' : 'weekly'],
    ];
    return h('section', { class: 'card' }, h('div', { class: 'card-head' }, h('h2', { text: 'Rules' })),
      h('dl', { class: 'kv' }, rows.flatMap(([k, v]) => [h('dt', { text: k }), h('dd', { text: v })])),
      h('p', { class: 'muted small', text: 'Change these with VPSW_ALERT_* in /etc/vps-watcher/vps-watcher.env, then restart the service.' }));
  }

  function channelStatus(st) {
    if (st.last_error_at && (!st.last_ok || st.last_error_at > st.last_ok)) {
      return h('p', { class: 'error small', text: `Last attempt failed ${fmt.ago(st.last_error_at)}: ${st.last_error}` });
    }
    if (st.last_ok) return h('p', { class: 'small', style: { color: 'var(--good)' }, text: `Last delivered ${fmt.ago(st.last_ok)} · ${st.sent} sent` });
    return h('p', { class: 'muted small', text: 'Nothing sent yet.' });
  }

  let lastResults = null;
  const resultLines = (list) => list.map((x) => h('p', { class: x.ok ? 'small' : 'error small', style: x.ok ? { color: 'var(--good)' } : null,
    text: `${x.channel === 'email' ? 'Email' : 'Webhook'}: ${x.ok ? 'sent. Check that it arrived.' : x.error}` }));
  function channelsCard(ch) {
    const results = h('div', { class: 'stack test-results', role: 'status' }, lastResults ? resultLines(lastResults) : null);
    const btn = h('button', { type: 'button', class: 'primary' }, icon('send'), 'Send test alert');
    btn.addEventListener('click', async () => {
      btn.disabled = true;
      fill(results, h('p', { class: 'muted small', text: 'Sending…' }));
      try {
        const r = await api.post('/api/alerts/test');
        lastResults = r.results;
        load(); // redraws the card with fresh delivery status and these results
      } catch (e) { btn.disabled = false; fill(results, h('p', { class: 'error small', text: e.message })); }
    });
    const email = ch.email
      ? [h('dl', { class: 'kv' },
        ...[['To', ch.email.to.join(', ')], ['From', ch.email.from], ['Server', `${ch.email.host}:${ch.email.port} · ${ch.email.security}`],
          ['Sign-in', { password: 'password', oauth2: 'OAuth2', none: 'none (relay)' }[ch.email.auth]]]
          .flatMap(([k, v]) => [h('dt', { text: k }), h('dd', { text: v })])), channelStatus(ch.email.status)]
      : [h('p', { class: 'muted small', text: 'Off. Set VPSW_SMTP_HOST, VPSW_SMTP_USER, VPSW_SMTP_PASSWORD and VPSW_ALERT_EMAIL.' })];
    const hook = ch.webhook
      ? [h('dl', { class: 'kv' },
        ...[['Host', ch.webhook.host], ['Format', ch.webhook.format], ['Signed', ch.webhook.signed ? 'yes (HMAC-SHA256)' : 'no']]
          .flatMap(([k, v]) => [h('dt', { text: k }), h('dd', { text: v })])), channelStatus(ch.webhook.status)]
      : [h('p', { class: 'muted small', text: 'Off. Set VPSW_WEBHOOK_URL (Slack, Discord, ntfy or your own endpoint).' })];
    return h('section', { class: 'card' }, h('div', { class: 'card-head' }, h('h2', { text: 'Where alerts go' })),
      h('h3', { text: 'Email' }), ...email, h('h3', { text: 'Webhook' }), ...hook,
      h('div', { class: 'row', style: { marginTop: '12px' } }, btn), results);
  }

  let rendered = false;
  async function load() {
    try {
      const d = await api.get('/api/alerts');
      paintActive(d.active);
      hist.set(d.history);
      if (!rendered) {
        rendered = true;
        fill(side, admin && d.channels ? channelsCard(d.channels) : null, rulesCard(d.rules));
      } else if (admin && d.channels) {
        side.firstChild.replaceWith(channelsCard(d.channels));
      }
    } catch (e) { fill(activeCard, h('p', { class: 'error', text: e.message })); }
  }
  state.onSnap = (s) => s && s.alerts && paintActive(s.alerts);
  await load();
  const iv = setInterval(() => { if (!document.hidden) api.get('/api/alerts').then((d) => hist.set(d.history)).catch(() => {}); }, 30000);
  state.cleanup.push(() => clearInterval(iv));
}

/* ---------- Reports ---------- */

function pageReports(view) {
  let range = store.get('report.range', '24h');
  const pick = rangePicker(range, (r) => { range = r; store.set('report.range', r); load(); });
  const csv = h('a', { class: 'btn', download: '' }, icon('download'), 'CSV');
  const print = h('button', { type: 'button', onclick: () => window.print() }, icon('printer'), 'Print / PDF');
  const mail = state.session.authenticated ? h('button', { type: 'button' }, icon('send'), 'Email report') : null;
  if (mail) {
    mail.addEventListener('click', async () => {
      mail.disabled = true;
      try { await api.post('/api/report/email', { range }); toast('Report emailed to the alert recipients.'); }
      catch (e) { toast(e.message); }
      mail.disabled = false;
    });
  }
  pageTitle(view, 'Reports');
  const out = h('div', { class: 'stack' });
  view.append(h('div', { class: 'toolbar' }, pick, h('span', { class: 'spacer' }), mail, csv, print), out);

  async function load() {
    csv.href = `/api/report.csv?range=${range}`;
    out.style.opacity = '.6';
    try {
      const r = await api.get(`/api/report?range=${range}`);
      fill(out, ...renderReport(r));
    } catch (e) { fill(out, h('p', { class: 'error', text: e.message })); }
    out.style.opacity = '';
  }
  load();
}

function renderReport(r) {
  const head = h('p', { class: 'muted' }, `${r.hostname} · last ${r.range} · generated ${fmt.stamp(r.generated)}. Peaks use averaged buckets (5 s for 1h, 1 min for 24h, 15 min beyond).`);
  const host = sortableTable([
    { key: 'label', label: 'Metric', get: (x) => x.label },
    { key: 'avg', label: 'Average', num: true, get: (x) => x.avg, render: (x) => formatter(x.unit)(x.avg) },
    { key: 'peak', label: 'Peak', num: true, get: (x) => x.peak, render: (x) => formatter(x.unit)(x.peak) },
    { key: 'last', label: 'Latest', num: true, get: (x) => x.last, render: (x) => formatter(x.unit)(x.last) },
  ], { id: 'rhost', defaultSort: 'order:asc' });
  host.set(r.host.map((x, i) => ({ ...x, order: i })));
  const apps = sortableTable([
    { key: 'name', label: 'App', get: (a) => a.name, render: (a) => nameCell(a.name, a.kind) },
    { key: 'cpu', label: 'CPU avg', num: true, get: (a) => a.cpu.avg, render: (a) => fmt.pct(a.cpu.avg) },
    { key: 'cpup', label: 'CPU peak', num: true, get: (a) => a.cpu.peak, render: (a) => fmt.pct(a.cpu.peak) },
    { key: 'mem', label: 'Memory avg', num: true, get: (a) => a.mem.avg, render: (a) => fmt.bytes(a.mem.avg) },
    { key: 'memp', label: 'Memory peak', num: true, get: (a) => a.mem.peak, render: (a) => fmt.bytes(a.mem.peak) },
    { key: 'rx', label: 'Received', num: true, get: (a) => a.net_rx, render: (a) => fmt.bytes(a.net_rx) },
    { key: 'tx', label: 'Sent', num: true, get: (a) => a.net_tx, render: (a) => fmt.bytes(a.net_tx) },
  ], { id: 'rapps', defaultSort: 'cpu:desc', empty: 'No app history yet.' });
  apps.set(r.apps);
  const availBadge = (v) => (v === null || v === undefined ? h('span', { class: 'badge', text: 'no data' })
    : h('span', { class: 'badge ' + (v >= 99.9 ? 'good' : v >= 99 ? 'warn' : 'bad'), text: fmt.num(v, v >= 99.9 ? 2 : 1) + '%' }));
  const mons = sortableTable([
    { key: 'name', label: 'Monitor', get: (m) => m.name, render: (m) => nameCell(m.name, m.kind) },
    { key: 'a', label: 'Availability', num: true, get: (m) => m.availability, render: (m) => availBadge(m.availability) },
    { key: 'ms', label: 'Avg response', num: true, get: (m) => m.avg_ms, render: (m) => (m.avg_ms === null ? '–' : `${Math.round(m.avg_ms)} ms`) },
  ], { id: 'rmons', defaultSort: 'a:asc', empty: 'No monitors.' });
  mons.set(r.monitors);
  const disks = sortableTable([
    { key: 'mount', label: 'Disk', get: (d) => d.mount },
    { key: 'used', label: 'Used now', num: true, get: (d) => d.used_pct, render: (d) => fmt.pct(d.used_pct) },
    { key: 'g', label: 'Growth per day', num: true, get: (d) => d.growth_per_day, render: (d) => (d.growth_per_day === null ? 'needs 6h of data' : `${d.growth_per_day >= 0 ? '+' : ''}${fmt.num(d.growth_per_day, 2)} pts`) },
    { key: 'full', label: 'Full in', num: true, get: (d) => d.days_to_full, render: (d) => (d.days_to_full === null ? (d.growth_per_day === null ? '–' : h('span', { class: 'badge good', text: 'not growing' })) : h('span', { class: 'badge ' + (d.days_to_full < 14 ? 'bad' : d.days_to_full < 60 ? 'warn' : 'good'), text: `~${Math.round(d.days_to_full)} days` })) },
  ], { id: 'rdisks', defaultSort: 'used:desc', empty: 'No disks.' });
  disks.set(r.disks);
  const card = (title, t) => h('section', { class: 'card' }, h('div', { class: 'card-head' }, h('h2', { text: title })), t.el);
  return [head, card('Server', host), card('Apps', apps), card('Availability', mons), card('Disk forecast', disks)];
}

/* ---------- Settings ---------- */

/** Current and latest version, when it was checked, and how upgrades happen. */
function updatesCard() {
  const card = h('section', { class: 'card' });
  const paint = (u) => {
    const st = u.status || {};
    const badge = !u.check ? h('span', { class: 'badge', text: 'checks off' })
      : st.available ? h('span', { class: 'badge warn', text: `${st.latest} available` })
        : st.error ? h('span', { class: 'badge bad', text: 'check failed' })
          : st.latest ? h('span', { class: 'badge good', text: 'up to date' })
            : h('span', { class: 'badge', text: 'not checked yet' });
    const btn = h('button', { type: 'button' }, icon('refresh'), 'Check now');
    btn.addEventListener('click', async () => {
      btn.disabled = true;
      try { const r = await api.post('/api/update/check'); state.session.update = r; paint(r); }
      catch (e) { toast(e.message); btn.disabled = false; }
    });
    const rows = [
      ['Running', st.current || state.session.version],
      ['Latest release', st.latest || '–'],
      ['Last checked', st.checked ? fmt.ago(st.checked) : '–'],
      ['Automatic install', u.auto ? 'on, signed releases only, once a day' : 'off'],
    ];
    fill(card,
      h('div', { class: 'card-head' }, h('h2', { text: 'Updates' }), badge),
      h('dl', { class: 'kv' }, rows.flatMap(([k, v]) => [h('dt', { text: k }), h('dd', { text: v })])),
      st.error ? h('p', { class: 'error small', text: st.error }) : null,
      h('p', { class: 'muted small', text: st.available
        ? (u.auto ? 'The daily timer installs it and restarts vps-watcher. To install now:' : 'To install it, run on the server:')
        : 'Upgrades are installed from the server, only when signed by the maintainer:' }),
      h('pre', { class: 'cmd', text: 'sudo vps-watcher upgrade' }),
      h('div', { class: 'row' }, u.check ? btn : null,
        st.latest ? h('a', { href: `${u.releases}/tag/${st.latest}`, target: '_blank', rel: 'noopener noreferrer', text: 'Release notes' }) : null));
  };
  paint(state.session.update || { status: {}, check: false });
  return card;
}

function pageSettings(view) {
  pageTitle(view, 'Settings');
  const s = state.session;
  const cur = h('input', { type: 'password', autocomplete: 'current-password', required: true });
  const email = h('input', { type: 'email', autocomplete: 'username', placeholder: s.email || '' });
  const pw = h('input', { type: 'password', autocomplete: 'new-password', minlength: '12' });
  const pw2 = h('input', { type: 'password', autocomplete: 'new-password' });
  const err = h('p', { class: 'error', role: 'alert' });
  const btn = h('button', { class: 'primary', type: 'submit', text: 'Save changes' });
  const form = h('form', { class: 'form' },
    field('New email', email, `Currently ${s.email || 'unknown'}. Leave empty to keep it.`),
    field('New password', pw, 'At least 12 characters. Leave empty to keep it.'), field('Confirm new password', pw2),
    field('Current password', cur, 'Required to save. Other browsers will be signed out.'), err, h('div', {}, btn));
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    err.textContent = '';
    if (pw.value !== pw2.value) { err.textContent = 'New passwords do not match.'; return; }
    if (!email.value && !pw.value) { err.textContent = 'Nothing to change.'; return; }
    btn.disabled = true;
    try {
      await api.post('/api/account', { current_password: cur.value, email: email.value || null, password: pw.value || null });
      toast('Saved. Other sessions were signed out.');
      state.session = await api.get('/api/session');
      route();
    } catch (x) { err.textContent = x.message; btn.disabled = false; }
  });
  const theme = h('div', { class: 'seg', role: 'group', 'aria-label': 'Theme' }, ['system', 'light', 'dark'].map((t) => {
    const b = h('button', { type: 'button', 'aria-pressed': String(store.get('theme', 'system') === t), text: t[0].toUpperCase() + t.slice(1) });
    b.addEventListener('click', () => { store.set('theme', t); applyTheme(); route(); });
    return b;
  }));
  const snap = state.snap || {};
  const about = h('dl', { class: 'kv' },
    ...[
      ['Version', s.version],
      ['Access mode', s.mode === 'open' ? 'open (public view)' : 'login required'],
      ['Sample interval', `${snap.interval || '–'} s`],
      ['cgroups', snap.cgroup_v2 ? 'v2 (unified)' : 'v1'],
      ['Docker', snap.docker ? (snap.docker.available ? 'connected' : snap.docker.error || 'not installed') : '–'],
      ['vps-watcher uses', snap.own ? `${fmt.bytes(snap.own.rss)} RAM · ${fmt.pct(snap.own.cpu_pct)} CPU` : '–'],
    ].flatMap(([k, v]) => [h('dt', { text: k }), h('dd', { text: v })]));
  view.append(h('div', { class: 'grid two' },
    h('section', { class: 'card' }, h('div', { class: 'card-head' }, h('h2', { text: 'Account' })), form),
    h('div', { class: 'stack' },
      h('section', { class: 'card' }, h('div', { class: 'card-head' }, h('h2', { text: 'Appearance' })), theme),
      h('section', { class: 'card' }, h('div', { class: 'card-head' }, h('h2', { text: 'Sessions' })),
        h('p', { class: 'muted small', text: 'Signed in somewhere you no longer use? End every other session.' }),
        h('button', { type: 'button', onclick: async () => { try { await api.post('/api/logout-others'); toast('Other sessions signed out.'); } catch (e) { toast(e.message); } } }, 'Sign out other browsers')),
      brandingCard(),
      updatesCard(),
      h('section', { class: 'card' }, h('div', { class: 'card-head' }, h('h2', { text: 'About' })), about,
        h('p', { class: 'small' }, h('a', { href: 'https://github.com/C3ZIZ/vps-watcher', target: '_blank', rel: 'noopener noreferrer', text: 'Documentation on GitHub' }))))));
}

boot();
