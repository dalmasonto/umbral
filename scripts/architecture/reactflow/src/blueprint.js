import { MarkerType } from '@xyflow/react';

// Detailed blueprint: five per-subsystem schematics that zoom into the
// mechanisms the high-level Flow only names. Same Card/Zone renderers; each
// section is a sequential pipeline of steps wired end-to-end.

let uid = 0;
const nodes = [];
const edges = [];
const ZX = 20, ZW = 1440, IM = 34, ZGAP = 64;
let Y = 40;

const COLOR = {
  mw: '#f472b6', runtime: '#5eead4', core: '#a78bfa', plugin: '#60a5fa',
  db: '#34d399', life: '#38bdf8', tool: '#94a3b8', coming: '#f59e0b', flow: '#7c6cf0',
};

function zone(id, cat, title, note, x, y, w, h) {
  nodes.push({
    id, type: 'zone', position: { x, y }, draggable: false, selectable: false,
    style: { width: w, height: h }, data: { cat, title, note }, zIndex: 0,
  });
}
function card(id, cat, kick, title, sub, x, y, w, h, parentId) {
  const n = {
    id, type: 'card', position: { x, y }, style: { width: w, height: h },
    data: { cat, kick, title, sub }, zIndex: 1,
  };
  if (parentId) { n.parentId = parentId; n.extent = 'parent'; }
  nodes.push(n);
}
function edge(source, sh, target, th, color, opts = {}) {
  edges.push({
    id: `b${uid++}`, source, target, sourceHandle: sh, targetHandle: th,
    type: 'smoothstep', animated: !!opts.anim,
    label: opts.label, data: { label: opts.label },
    labelStyle: { fill: '#c7cde0', fontFamily: 'ui-monospace, monospace', fontSize: 10, fontWeight: 600 },
    labelBgStyle: { fill: '#070912', fillOpacity: 0.92 }, labelBgPadding: [6, 3], labelBgBorderRadius: 5,
    style: { stroke: color, strokeWidth: opts.w || 1.8, strokeDasharray: opts.dash ? '6 5' : undefined },
    markerEnd: { type: MarkerType.ArrowClosed, color, width: 14, height: 14 }, zIndex: 2,
  });
}

// Lay `steps` in a wrapped grid and wire them end-to-end (L→R within a row,
// then a dashed wrap down to the next row) — a serpentine pipeline.
function pipeline(id, cat, title, note, steps, o) {
  const cols = o.cols, cardH = o.cardH, gapX = o.gapX, gapY = o.gapY, top = o.top, botPad = o.botPad || 34;
  const cardW = (ZW - 2 * IM - (cols - 1) * gapX) / cols;
  const rows = Math.ceil(steps.length / cols);
  const h = top + rows * cardH + (rows - 1) * gapY + botPad;
  zone(id, cat, title, note, ZX, Y, ZW, h);
  // boustrophedon: even rows left→right, odd rows right→left, so every step
  // joins one continuous snake (→ ↓ ← ↓ →) with straight vertical drops.
  const visualCol = (i) => {
    const r = Math.floor(i / cols), inRow = i % cols;
    return r % 2 === 0 ? inRow : cols - 1 - inRow;
  };
  const ids = steps.map((s, i) => {
    const r = Math.floor(i / cols);
    const nid = `${id}_${i}`;
    card(nid, s.cat || cat, s.k, s.t, s.s || '', IM + visualCol(i) * (cardW + gapX), top + r * (cardH + gapY), cardW, cardH, id);
    return nid;
  });
  const line = COLOR[cat] || '#5b6480';
  for (let i = 0; i < steps.length - 1; i++) {
    const r = Math.floor(i / cols);
    if (i % cols === cols - 1) edge(ids[i], 'bs', ids[i + 1], 'tt', line, { w: 1.8 }); // drop straight down
    else if (r % 2 === 0) edge(ids[i], 'rs', ids[i + 1], 'lt', line, { anim: true }); // → even row
    else edge(ids[i], 'ls', ids[i + 1], 'rt', line, { anim: true }); // ← odd row
  }
  Y += h + ZGAP;
}

// ── 1 · Request pipeline ────────────────────────────────────────────────
pipeline('req', 'life', 'Request pipeline', 'the tower Layer chain, in order · pink = middleware', [
  { k: 'ACCEPT', t: 'TCP · TLS', s: 'hyper accepts the socket' },
  { k: 'LAYER', t: 'Trace span', s: 'request span · traceparent', cat: 'mw' },
  { k: 'LAYER · MW', t: 'Security', s: 'CSRF verify · HSTS/CSP/frame', cat: 'mw' },
  { k: 'LAYER · MW', t: 'Host validation', s: 'allowed_hosts guard', cat: 'mw' },
  { k: 'LAYER · MW', t: 'Sessions', s: 'cookie → store load', cat: 'mw' },
  { k: 'LAYER · MW', t: 'User-context', s: 'resolve user → templates', cat: 'mw' },
  { k: 'ROUTE', t: 'Router match', s: 'axum Router → handler' },
  { k: 'EXTRACT', t: 'Extractors', s: 'Path/Query/Json/Form + CSRF' },
  { k: 'HANDLER', t: 'Handler / viewset', s: 'REST · GraphQL · view fn' },
  { k: 'DATA', t: 'ORM', s: 'QuerySet → SQL → rows', cat: 'runtime' },
  { k: 'RENDER', t: 'Response', s: 'serializer / autoescaped HTML' },
  { k: 'LAYER · MW', t: 'Response headers', s: 'set on the way out', cat: 'mw' },
], { cols: 4, cardH: 94, gapX: 30, gapY: 50, top: 88 });

// ── 2 · ORM: QuerySet → SQL ─────────────────────────────────────────────
pipeline('orm', 'runtime', 'ORM — QuerySet → SQL', 'Model to rows, always parameterized', [
  { k: 'ENTRY', t: 'Model::objects()', s: 'QuerySet<Model>' },
  { k: 'BUILD', t: 'filter · order · limit', s: '.filter(col.eq(x)) …' },
  { k: 'BUILD', t: 'select_related', s: 'joins · avoids N+1' },
  { k: 'COMPILE', t: 'query AST', s: 'sea-query builder' },
  { k: 'EXEC', t: 'terminal', s: '.fetch / .first / .count' },
  { k: 'DISPATCH', t: 'pool_dispatched()', s: 'pick backend + pool' },
  { k: 'EMIT', t: 'SQL + binds', s: 'parameterized · backend-specific' },
  { k: 'DRIVER', t: 'sqlx execute', s: 'prepared statement' },
  { k: 'MAP', t: 'hydrate', s: 'rows → Model (FromRow)' },
], { cols: 4, cardH: 92, gapX: 30, gapY: 48, top: 88 });

// ── 3 · Migration engine ────────────────────────────────────────────────
pipeline('mig', 'core', 'Migration engine', 'declare → makemigrations → migrate · autodetect + tracking table', [
  { k: 'DECLARE', t: '#[derive(Model)]', s: 'declare / change a field' },
  { k: 'CLI', t: 'makemigrations', s: 'load the last snapshot' },
  { k: 'AUTODETECT', t: 'diff', s: 'models vs snapshot → ops' },
  { k: 'GATE', t: 'UnsafeAlter guard', s: 'refuse NOT NULL w/o default', cat: 'mw' },
  { k: 'EMIT', t: 'write migration', s: 'NNNN_name.json + snapshot' },
  { k: 'CLI', t: 'migrate', s: 'ensure tracking table' },
  { k: 'VERIFY', t: 'drift check', s: 'applied set vs on-disk' },
  { k: 'APPLY', t: 'apply pending', s: 'DDL in FK order · PG advisory lock' },
  { k: 'LEDGER', t: 'record', s: 'row in the tracking table' },
], { cols: 4, cardH: 92, gapX: 30, gapY: 48, top: 88 });

// ── 4 · Plugin trait dispatch + boot lifecycle ──────────────────────────
pipeline('plug', 'plugin', 'Plugin trait — build & boot lifecycle', 'Box<dyn Plugin> registry · deps point inward', [
  { k: 'WIRE', t: 'App::builder()', s: 'start the builder' },
  { k: 'REGISTER', t: '.plugin(P)', s: 'collect Box<dyn Plugin>' },
  { k: 'PHASE 1', t: 'collect models()', s: '→ migrate registry', cat: 'core' },
  { k: 'PHASE 2', t: 'routes · cmds · settings', s: '+ admin registrations' },
  { k: 'ORDER', t: 'topo-sort', s: 'by plugin deps (FK order)' },
  { k: 'PHASE 3', t: 'set ambient DbPool', s: 'OnceLock — the one global', cat: 'runtime' },
  { k: 'PHASE 4', t: 'system_checks()', s: 'Error blocks · Warning logs', cat: 'mw' },
  { k: 'PHASE 5', t: 'on_ready()', s: 'per plugin, in order' },
  { k: 'PHASE 6', t: 'wrap_router()', s: 'compose tower layers', cat: 'mw' },
  { k: 'RUN', t: 'serve', s: 'axum listens' },
], { cols: 4, cardH: 92, gapX: 30, gapY: 48, top: 88 });

// ── 5 · DbPool routing + RLS/GUC ────────────────────────────────────────
pipeline('rls', 'db', 'DbPool routing + RLS', 'read/write split · db-per-tenant · Postgres RLS via GUCs', [
  { k: 'CONTEXT', t: 'request identity', s: 'user_id · tenant' },
  { k: 'ROUTE', t: 'DatabaseRouter', s: 'pick alias: default / replica / tenant' },
  { k: 'DISPATCH', t: 'pool_dispatched()', s: 'Sqlite | Postgres' },
  { k: 'POOL', t: 'checkout conn', s: 'sqlx connection pool' },
  { k: 'SESSION', t: 'SET GUC', s: 'app.user_id · app.tenant (PG)' },
  { k: 'GUARD', t: 'RLS policies', s: 'USING(…) filters rows (PG only)', cat: 'mw' },
  { k: 'EXEC', t: 'run query', s: 'writes→default · reads→replica' },
  { k: 'CLEANUP', t: 'release', s: 'reset umbra GUCs on checkin' },
], { cols: 4, cardH: 92, gapX: 30, gapY: 48, top: 88 });

export const blueprintNodes = nodes;
export const blueprintEdges = edges;
export const blueprintLegend = [
  ['Middleware', COLOR.mw], ['Data / ORM', COLOR.runtime], ['Core', COLOR.core],
  ['Plugin', COLOR.plugin], ['Database', COLOR.db], ['Lifecycle', COLOR.life],
];
