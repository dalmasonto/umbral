import { MarkerType } from '@xyflow/react';

// ---------- builders ----------
let uid = 0;
const nodes = [];
const edges = [];

const KICK = {
  tool: 'TOOLING', facade: 'CRATE', core: 'CORE', plugin: 'PLUGIN',
  coming: 'PLANNED', runtime: 'SERVICE', db: 'BACKEND', mw: 'LAYER', life: 'PHASE', loop: 'STEP',
};

function zone(id, cat, title, note, x, y, w, h, opts = {}) {
  nodes.push({
    id, type: 'zone', position: { x, y }, draggable: false, selectable: false,
    style: { width: w, height: h }, data: { cat, title, note, dashed: !!opts.dashed }, zIndex: 0,
  });
}
function card(id, cat, title, sub, desc, x, y, w, h, parentId, opts = {}) {
  const n = {
    id, type: 'card', position: { x, y }, style: { width: w, height: h },
    data: { cat, kick: opts.kick || KICK[cat] || '', title, sub, desc, dashed: !!opts.dashed, big: !!opts.big },
    zIndex: 1,
  };
  if (parentId) { n.parentId = parentId; n.extent = 'parent'; }
  nodes.push(n);
}
const COLOR = {
  tool: '#94a3b8', facade: '#8b7bf5', core: '#a78bfa', plugin: '#60a5fa',
  runtime: '#5eead4', db: '#34d399', mw: '#f472b6', coming: '#f59e0b',
  life: '#38bdf8', loop: '#c4b5fd', flow: '#7c6cf0',
};
function edge(source, sh, target, th, opts = {}) {
  const color = opts.color || '#39406a';
  edges.push({
    id: `e${uid++}`, source, target, sourceHandle: sh, targetHandle: th,
    type: opts.type || 'smoothstep', animated: !!opts.anim,
    label: opts.label, data: { label: opts.label }, labelShowBg: true,
    labelStyle: { fill: '#c7cde0', fontFamily: 'ui-monospace, monospace', fontSize: 10, fontWeight: 600 },
    labelBgStyle: { fill: '#070912', fillOpacity: 0.92 },
    labelBgPadding: [7, 4], labelBgBorderRadius: 6,
    style: { stroke: color, strokeWidth: opts.w || 2, strokeDasharray: opts.dash ? '6 5' : undefined },
    markerEnd: { type: MarkerType.ArrowClosed, color, width: 15, height: 15 },
    zIndex: 2,
  });
}

// ---------- layout engine: a running cursor + a grid helper ----------
// Every left-column region stacks under the previous one with a fixed gap, and
// cards inside are placed on an even grid — so spacing is uniform and tunable.
const ZX = 20, ZW = 1440, IM = 34, ZGAP = 60;
let Y = 316; // cursor for the left column (below the developer/facade rows)

function region(id, cat, title, note, specs, o) {
  const cols = o.cols, cardH = o.cardH, gapX = o.gapX, gapY = o.gapY || 0, top = o.top, botPad = o.botPad || 34;
  const cardW = (ZW - 2 * IM - (cols - 1) * gapX) / cols;
  const rows = Math.ceil(specs.length / cols);
  const h = top + rows * cardH + (rows - 1) * gapY + botPad;
  zone(id, cat, title, note, ZX, Y, ZW, h, { dashed: o.dashed });
  specs.forEach((s, i) => {
    const c = i % cols, r = Math.floor(i / cols);
    card(s.id, s.cat || cat, s.title, s.sub || '', s.desc || '',
      IM + c * (cardW + gapX), top + r * (cardH + gapY), cardW, cardH, id,
      { kick: s.kick, dashed: o.dashed || s.dashed, big: o.big });
  });
  Y += h + ZGAP;
}

// ---------- top row: developer surface ----------
card('app', 'tool', 'Your app crate', 'App::builder().plugin(…).build()', 'core + every chosen plugin · explicit wiring', 40, 24, 380, 108, null, { kick: 'BINARY' });
card('cli', 'tool', 'umbral-cli', 'migrate · makemigrations · worker · inspectdb', 'startproject · startplugin · typegen', 1000, 24, 430, 108, null, { kick: 'TOOLING' });
card('facade', 'facade', 'umbral — the facade', 'use umbral::prelude::*', 'the one stable surface · re-exports core + macros', 40, 186, 900, 94, null, { kick: 'FACADE', big: true });
card('macros', 'facade', 'umbral-macros', '#[derive(Model)] · #[task]', 'generates the Model / QuerySet impls', 1000, 186, 430, 94, null, { kick: 'PROC-MACRO' });

// ---------- core ----------
region('core', 'core', 'umbral-core', 'thin core · deps point inward · control flows out via the Plugin trait', [
  { id: 'plugintrait', cat: 'core', kick: 'TRAIT · SEAM', title: 'Plugin trait', sub: 'Box<dyn Plugin> registry', desc: 'models · routes · middleware · commands · settings · admin · on_ready · system_checks' },
  { id: 'router', cat: 'plugin', kick: 'ROUTING', title: 'Router', sub: 'axum · builds the tower stack', desc: 'delegates the pipeline to the Middleware layer ↓' },
  { id: 'orm', cat: 'runtime', kick: 'PERSISTENCE', title: 'ORM', sub: 'Model → QuerySet → SQL', desc: 'filter · select_related · annotate · bulk · tx · always parameterized' },
  { id: 'migrate', cat: 'core', kick: 'SCHEMA', title: 'Migration engine', sub: 'snapshot · autodetect · tracking table', desc: 'create/alter/drop · reversible · cross-plugin FK · inspectdb' },
  { id: 'backend', cat: 'plugin', kick: 'ABSTRACTION', title: 'Backend abstraction', sub: 'DatabaseBackend · boot checks', desc: 'field/backend compatibility caught at boot · Postgres-first' },
  { id: 'pool', cat: 'runtime', kick: 'AMBIENT STATE', title: 'Ambient DbPool', sub: 'OnceLock — the one global', desc: 'pool_dispatched() picks the backend' },
], { cols: 3, cardH: 168, gapX: 36, gapY: 48, top: 94, botPad: 44, big: true });

// ---------- middleware pipeline (ordered — the point of #102) ----------
// 3 + 2 layout so each ordered arrow has room to flow; LAYER 01→02→03 then a
// "next" connector wraps down to 04→05, like text wrapping to the next line.
region('mw', 'mw', 'Middleware pipeline', 'tower Layer chain · ordered · wrap_router seam · gaps5 #102 opens ordering / per-route / typed layers', [
  { id: 'mwSec', kick: 'LAYER 01', title: 'Security', sub: 'CSRF · HSTS · headers' },
  { id: 'mwHost', kick: 'LAYER 02', title: 'Host validation', sub: 'allowed_hosts guard' },
  { id: 'mwSess', kick: 'LAYER 03', title: 'Sessions', sub: 'store · signed cookie' },
  { id: 'mwUser', kick: 'LAYER 04', title: 'User-context', sub: 'user injected into templates' },
  { id: 'mwHandler', kick: 'LAYER 05', title: 'Handler / viewset', sub: 'REST · GraphQL · view' },
], { cols: 3, cardH: 100, gapX: 44, gapY: 44, top: 90, botPad: 34 });

// ---------- built-in plugins ----------
const shipped = [
  ['umbral-auth', 'users · argon2 · perms'], ['umbral-sessions', 'store + middleware'],
  ['umbral-admin', 'auto CRUD UI · views'], ['umbral-tasks', 'DB-backed queue'],
  ['umbral-rest', 'serializers · viewsets'], ['umbral-graphql', 'expose · mutable'],
  ['umbral-openapi', 'schema · Swagger UI'], ['umbral-storage', 'FS · S3 · signed URLs'],
  ['umbral-realtime', 'WS · presence'], ['umbral-cache', 'ambient cache'],
  ['umbral-security', 'CSRF · HSTS · headers'], ['umbral-email', 'send · templates'],
  ['umbral-tenants', 'multi-tenancy'], ['umbral-rls', 'row-level security'],
  ['umbral-logs', 'request logs'], ['umbral-analytics', 'events · PostHog'],
  ['umbral-oauth', 'Google · GitHub'], ['umbral-health', 'readiness · liveness'],
  ['umbral-playground', 'API explorer'], ['umbral-permissions', 'roles · object perms'],
  ['umbral-signals', 'pre/post-save hooks'], ['umbral-livereload', 'dev autoreload'],
];
region('plugins', 'plugin', 'Built-in plugins', 'each its own crate · structurally identical to a third-party plugin · a REST-free app compiles with zero serializer code',
  shipped.map(([t, s], i) => ({ id: 'sp' + i, title: t, sub: s })),
  { cols: 5, cardH: 90, gapX: 38, gapY: 30, top: 92, botPad: 36 });

// ---------- roadmap (dashed region — not built yet) ----------
const coming = [
  ['feature flags', 'rollout · targeting'], ['metering & billing', 'quotas · usage'],
  ['i18n / l10n', 'locale · plurals'], ['edge functions', 'deployable units'],
  ['policy engine', 'ABAC · named policies'], ['object-scope check', 'IDOR guard · #101'],
  ['typed middleware', 'ordering · #102'], ['compliance', 'DSAR · retention'],
  ['tamper-evident audit', 'hash-chained log'], ['OIDC · SAML · SCIM', 'enterprise identity'],
  ['task broker / DLQ', 'durable queue'], ['CDC outbox', 'after-commit events'],
  ['DB branching · PITR', 'preview · backup'],
];
region('roadmap', 'coming', 'Roadmap', 'not limited to what ships today · each lands as a plugin, the same contract as the built-ins',
  coming.map(([t, s], i) => ({ id: 'cm' + i, title: t, sub: s })),
  { cols: 5, cardH: 92, gapX: 34, gapY: 30, top: 82, botPad: 34, dashed: true });

// ---------- runtime services ----------
const services = [
  ['Tasks worker', 'polls DB queue · runs #[task]s'], ['Realtime', 'WS · presence · channels'],
  ['Email', 'send now · retry (coming)'], ['Cache', 'get/set · invalidation'],
  ['Storage', 'FS / S3 · gated media · signed URLs'],
];
region('runtime', 'runtime', 'Runtime services', 'all ride the ambient ORM / DbPool path',
  services.map(([t, s], i) => ({ id: 'svc' + i, title: t, sub: s })),
  { cols: 5, cardH: 94, gapX: 40, top: 80, botPad: 34 });

// ---------- databases (router → backends; replicas/backup partial, CDC on the roadmap) ----------
{
  const h = 556;
  zone('db', 'db', 'Databases', 'Postgres-first · SQLite for tests · read replicas (beta) · one ORM path via DatabaseRouter', ZX, Y, ZW, h);
  card('dispatch', 'db', 'DbPool · DatabaseRouter', 'pool_dispatched() · database(alias, pool)', 'read/write split · db-per-tenant · transaction_on(alias)', 380, 72, 680, 90, 'db', { kick: 'DISPATCH · ROUTING', big: true });
  card('pg', 'db', 'PostgreSQL', 'RLS · matviews · arrays · citext · JSON', 'the production write backend', 40, 236, 440, 108, 'db', { kick: 'PRIMARY · WRITES', big: true });
  card('replica', 'db', 'Read replicas', 'read routing · failover', 'wired via DatabaseRouter — not yet fully tested', 500, 292, 440, 108, 'db', { kick: 'REPLICA · BETA', big: true });
  card('sqlite', 'db', 'SQLite', 'single-writer · zero-config', 'same ORM path — mismatches caught at boot', 960, 236, 440, 108, 'db', { kick: 'TEST · DEV', big: true });
  card('backup', 'db', 'Backup · snapshots', 'backup.rs · dump / restore', 'PITR runbook — partial', 40, 428, 680, 84, 'db', { kick: 'DURABLE' });
  card('cdc', 'db', 'CDC · outbox · branching', 'after-commit events · preview DBs · PITR', 'designed — see Roadmap', 760, 428, 660, 84, 'db', { kick: 'ROADMAP', dashed: true });
  Y += h + ZGAP;
}

// ---------- right column: cursor + single-column region helper ----------
const RX = 1500, RW = 400, RIM = 24, RGAP = 56;
let YR = 24;
function vregion(id, cat, title, note, specs, o) {
  const cardW = RW - 2 * RIM;
  const h = o.top + specs.length * o.cardH + (specs.length - 1) * o.gapY + (o.botPad || 40);
  zone(id, cat, title, note, RX, YR, RW, h, { dashed: o.dashed });
  specs.forEach((s, i) => card(s.id, s.cat || cat, s.title, s.sub || '', s.desc || '',
    RIM, o.top + i * (o.cardH + o.gapY), cardW, o.cardH, id, { kick: s.kick, dashed: o.dashed || s.dashed }));
  YR += h + RGAP;
}

const steps = [
  ['HTTP request', 'axum accepts the connection'],
  ['Security middleware', 'CSRF · HSTS · frame · headers'],
  ['Host validation', 'allowed_hosts guard'],
  ['Sessions + user-context', 'user injected into templates'],
  ['Router → handler', 'or a REST / GraphQL viewset'],
  ['ORM QuerySet', 'built, then parameterized SQL'],
  ['Backend dispatch', 'Postgres / SQLite via DbPool'],
  ['Response', 'serializer / autoescaped HTML'],
];
vregion('life', 'life', 'Request lifecycle', '',
  steps.map(([t, s], i) => ({ id: 'lf' + i, kick: 'PHASE 0' + (i + 1), title: t, sub: s })),
  { cardH: 116, gapY: 44, top: 92, botPad: 40 });

// each step shows its actual command (mono sub) + a plain-language note (desc)
const loop = [
  { id: 'lp0', kick: 'STEP 01', title: 'Declare / change', sub: '#[derive(Model)] struct Post { … }', desc: 'add, change, or drop a field' },
  { id: 'lp1', kick: 'STEP 02', title: 'makemigrations', sub: 'cargo run -- makemigrations', desc: 'diff current models vs the last snapshot' },
  { id: 'lp2', kick: 'STEP 03', title: 'migrate', sub: 'cargo run -- migrate', desc: 'apply pending · record in the tracking table' },
  { id: 'lp3', kick: 'STEP 04', title: 'Change again', sub: 'edit the model, then makemigrations', desc: 'the diff writes the right ALTER / DROP' },
  { id: 'lpnote', kick: 'RULE', title: 'never wipe the DB', sub: 'existing rows are the test', dashed: true },
];
vregion('loop', 'loop', 'The everyday loop', 'declare → migrate → change → migrate · this cycle IS the product',
  loop, { cardH: 150, gapY: 40, top: 100, botPad: 40 });

// ---------- right column: posture cards (fill + say something true) ----------
card('secure', 'plugin', 'Secure by default', 'CSRF · clickjacking / HSTS · template autoescaping', 'always-parameterized SQL · backend mismatches caught at boot', RX, YR, RW, 158, null, { kick: 'POSTURE' });
card('principle', 'core', 'Thin core, plugin-heavy', 'the framework dogfoods its own plugin system', 'deps point inward · control flows out through the Plugin trait', RX, YR + 182, RW, 158, null, { kick: 'ONE IDEA' });
card('backends', 'db', 'One ORM path, many databases', 'read replicas (beta) · db-per-tenant · backup', 'routed by DatabaseRouter — Postgres-first, SQLite for tests', RX, YR + 364, RW, 158, null, { kick: 'DATA LAYER' });

// ---------- edges ----------
edge('app', 'bs', 'facade', 'tt', { label: 'wires plugins', color: COLOR.tool });
edge('facade', 'bs', 'core', 'tt', { label: 'imports', color: COLOR.facade, w: 2.5 });
edge('cli', 'bs', 'migrate', 'tt', { label: 'drives migrate · worker', color: COLOR.tool, dash: true });
edge('macros', 'bs', 'orm', 'tt', { label: 'derive', color: COLOR.facade, dash: true });

// core → middleware pipeline
edge('router', 'bs', 'mwSec', 'tt', { label: 'builds the tower stack', color: COLOR.mw, w: 2.5 });
edge('mwSec', 'rs', 'mwHost', 'lt', { color: COLOR.mw, anim: true });
edge('mwHost', 'rs', 'mwSess', 'lt', { color: COLOR.mw, anim: true });
edge('mwSess', 'bs', 'mwUser', 'tt', { color: COLOR.mw, anim: true, label: 'next' }); // wrap to row 2
edge('mwUser', 'rs', 'mwHandler', 'lt', { color: COLOR.mw, anim: true });

// plugins register through the trait — routed up the RIGHT gutter (clear of the middleware band)
// label omitted: it overflowed the narrow gutter into the lifecycle panel; the
// animated edge into the Plugin trait node conveys registration on its own.
edge('plugins', 'rs', 'core', 'rt', { color: COLOR.plugin, w: 2.5, anim: true });

// the one ORM path → DB — custom umbra-spine down the far LEFT margin
edge('pool', 'bs', 'dispatch', 'lt', { label: 'pool_dispatched()', color: COLOR.flow, w: 3, type: 'spine' });
edge('dispatch', 'bs', 'pg', 'tt', { color: COLOR.db, w: 2.5, label: 'writes' });
edge('dispatch', 'bs', 'replica', 'tt', { color: COLOR.db, w: 2, label: 'reads' });
edge('dispatch', 'bs', 'sqlite', 'tt', { color: COLOR.db, w: 2.5 });
// dashed, no label: the short pg→replica hop had no room for a label without
// clipping the replica card; the dashed edge reads as replication on its own.
edge('pg', 'rs', 'replica', 'lt', { color: COLOR.db, w: 1.6, dash: true });

// lifecycle chain
for (let i = 0; i < steps.length - 1; i++) edge('lf' + i, 'bs', 'lf' + (i + 1), 'tt', { color: COLOR.life });

// loop chain + loopback
for (let i = 0; i < loop.length - 1; i++) edge('lp' + i, 'bs', 'lp' + (i + 1), 'tt', { color: COLOR.loop });
edge('lp3', 'ls', 'lp0', 'lt', { label: 'repeat', color: COLOR.loop, dash: true });

export const initialNodes = nodes;
export const initialEdges = edges;
export const legendItems = [
  ['Thin core', COLOR.core], ['Built-in plugin', COLOR.plugin], ['Middleware', COLOR.mw],
  ['Runtime service', COLOR.runtime], ['Database', COLOR.db], ['Lifecycle', COLOR.life],
  ['CLI / tooling', COLOR.tool], ['Roadmap (dashed)', COLOR.coming],
];
