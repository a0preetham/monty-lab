// Experiment F — one Durable Object per agent run, named by run ID.
//
// The object's globally unique name IS the run ID, so anyone holding the ID can
// reach that exact object from anywhere — no allocation, no lookup table.
//
// Warm path : the object stays awake, the Monty session sits in memory, nothing
//             is serialized per turn.
// Cold path : the object was evicted; it reads its snapshot back out of its own
//             SQLite storage and keeps feeding.
//
// Cloudflare does not snapshot object memory — hibernation discards it — so the
// snapshot written to storage is the only thing that survives.

import { DEMO_HTML } from './demo.js';
import initWasm, { AgentSession } from '../wasm/pkg/monty_session.js';
// Workers resolves a .wasm import to a WebAssembly.Module (compiled at deploy
// time, not per request). wasm-bindgen's `web` target init accepts one, so no
// fetch/streaming path is needed — which is what makes this work on Workers.
import wasmModule from '../wasm/pkg/monty_session_bg.wasm';

// ---------------------------------------------------------------------------
// Tools. Async on purpose: this is the whole reason the interpreter exposes a
// step machine instead of taking a synchronous callback.
// ---------------------------------------------------------------------------

const TOOLS = {
  async now() {
    return new Date().toISOString().slice(0, 10);
  },
  async double(n) {
    return n * 2;
  },
  // A tool that reads the object's own SQLite storage — i.e. a real async
  // capability the guest could not have without the host.
  async remember(ctx, key, value) {
    ctx.sql.exec('INSERT OR REPLACE INTO notes (k, v) VALUES (?, ?)', key, JSON.stringify(value));
    return `stored ${key}`;
  },
  async recall(ctx, key) {
    const rows = [...ctx.sql.exec('SELECT v FROM notes WHERE k = ?', key)];
    return rows.length ? JSON.parse(rows[0].v) : null;
  },
  async fail() {
    throw new Error('this tool always fails');
  },
};

const NEEDS_CTX = new Set(['remember', 'recall']);

// Tools the host deliberately does NOT answer inline. Hitting one parks the run
// mid-call: the snapshot is written while suspended and the object can hibernate
// for as long as it takes a human to answer. This is the durable-execution case
// the whole design exists for.
const PARK_TOOLS = new Set(['await_approval']);

// ---------------------------------------------------------------------------

export class AgentRun {
  constructor(state, env) {
    this.state = state;
    this.env = env;
    this.sql = state.storage.sql;
    this.session = null;       // the warm interpreter, or null when cold
    this.loadedFrom = null;    // 'memory' | 'snapshot' | 'new'
    this.wasmReady = null;

    // Schema lives in a blockConcurrencyWhile so no request sees a half-made DB.
    state.blockConcurrencyWhile(async () => {
      this.sql.exec(`CREATE TABLE IF NOT EXISTS snapshot (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        bytes BLOB NOT NULL,
        dump_version INTEGER NOT NULL,
        turns INTEGER NOT NULL,
        updated_at TEXT NOT NULL
      )`);
      this.sql.exec(`CREATE TABLE IF NOT EXISTS notes (k TEXT PRIMARY KEY, v TEXT NOT NULL)`);
      this.sql.exec(`CREATE TABLE IF NOT EXISTS log (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        at TEXT NOT NULL,
        event TEXT NOT NULL
      )`);
    });
  }

  async ensureWasm() {
    if (!this.wasmReady) this.wasmReady = initWasm({ module_or_path: wasmModule });
    await this.wasmReady;
  }

  log(event) {
    this.sql.exec('INSERT INTO log (at, event) VALUES (?, ?)', new Date().toISOString(), event);
  }

  /** Warm if possible, otherwise rehydrate from this object's own storage. */
  async ensureSession() {
    // Deploy-measurement instrumentation: separates the three costs that make
    // up a cold wake (wasm init, snapshot read, Dump::load) so the numbers can
    // be attributed rather than lumped into one latency figure.
    this.timings = {};
    const tWasm = Date.now();
    await this.ensureWasm();
    this.timings.wasmInitMs = Date.now() - tWasm;
    if (this.session) {
      this.loadedFrom = 'memory';
      return this.session;
    }
    const tRead = Date.now();
    const rows = [...this.sql.exec('SELECT bytes, dump_version, turns FROM snapshot WHERE id = 1')];
    this.timings.sqlReadMs = Date.now() - tRead;
    if (rows.length) {
      const stored = rows[0].dump_version;
      const mine = AgentSession.dumpVersion();
      if (stored !== mine) {
        // Experiment D: this is where a multi-version build would dispatch to
        // the interpreter that can read `stored` instead of failing.
        throw new Error(`snapshot is dump v${stored}, this build reads v${mine}`);
      }
      const tLoad = Date.now();
      this.session = AgentSession.restore(new Uint8Array(rows[0].bytes), undefined);
      this.timings.restoreMs = Date.now() - tLoad;
      this.timings.snapshotBytes = rows[0].bytes.byteLength ?? rows[0].bytes.length;
      // The count lives in the snapshot row, so carry it across a cold wake --
      // otherwise a resumed run reports turn 1 and looks like a new session.
      this.turnCount = rows[0].turns ?? 0;
      this.loadedFrom = 'snapshot';
      this.log('woke from snapshot');
    } else {
      this.session = new AgentSession('agent-run.py', undefined);
      this.loadedFrom = 'new';
      this.log('created');
    }
    return this.session;
  }

  persist() {
    const bytes = this.session.snapshot();
    const turns = this.turnCount ?? 0;
    this.sql.exec(
      `INSERT INTO snapshot (id, bytes, dump_version, turns, updated_at) VALUES (1, ?, ?, ?, ?)
       ON CONFLICT(id) DO UPDATE SET bytes = excluded.bytes, dump_version = excluded.dump_version,
                                     turns = excluded.turns, updated_at = excluded.updated_at`,
      bytes, AgentSession.dumpVersion(), turns, new Date().toISOString(),
    );
    return bytes.length;
  }

  /** Run one turn: feed a snippet, service tool calls, persist. */
  async runTurn(code) {
    const session = await this.ensureSession();
    const tFeed = Date.now();
    const first = session.start(code);
    this.timings.firstStepMs = Date.now() - tFeed;
    return this.drive(session, first, []);
  }

  /** Answer a parked tool call and carry on from where the run suspended. */
  async resumeParked(value) {
    const session = await this.ensureSession();
    if (!session.suspended) throw new Error('this run is not parked');
    return this.drive(session, session.resume(value), []);
  }

  /** The suspension loop, shared by a fresh turn and a resumed one. */
  async drive(session, first, toolCalls) {
    let step = first;

    while (step.kind === 'tool' || step.kind === 'name') {
      if (step.kind === 'tool' && PARK_TOOLS.has(step.name)) {
        // Persist *while suspended* and hand control back to the caller.
        const snapshotBytes = this.persist();
        this.log(`parked at ${step.name}`);
        return {
          step: { kind: 'parked', name: step.name, args: step.args },
          toolCalls,
          loadedFrom: this.loadedFrom,
          snapshotBytes,
          suspended: session.suspended,
          turns: this.turnCount ?? 0,
        };
      }
      if (step.kind === 'name') {
        // Bare undeclared name; the interpreter caches whatever we return, so
        // only constants belong here.
        step = session.resumeNotFound();
        continue;
      }
      const tool = TOOLS[step.name];
      if (!tool) {
        toolCalls.push({ name: step.name, outcome: 'not available' });
        step = session.resumeNotFound();
        continue;
      }
      try {
        const args = NEEDS_CTX.has(step.name) ? [{ sql: this.sql }, ...step.args] : step.args;
        const result = await tool(...args);   // genuinely async
        toolCalls.push({ name: step.name, args: step.args, result });
        step = session.resume(result);
      } catch (e) {
        toolCalls.push({ name: step.name, args: step.args, error: String(e.message ?? e) });
        step = session.resumeError(String(e.message ?? e));
      }
    }

    this.turnCount = (this.turnCount ?? 0) + 1;
    const tPersist = Date.now();
    const snapshotBytes = this.persist();
    if (this.timings) this.timings.persistMs = Date.now() - tPersist;
    this.log(`turn ${this.turnCount}: ${step.kind}`);

    return {
      step,
      toolCalls,
      loadedFrom: this.loadedFrom,
      snapshotBytes,
      suspended: session.suspended,
      turns: this.turnCount,
      timings: this.timings,
    };
  }

  /** Park the run until `alarmAt`, then resume it without an inbound request. */
  async scheduleResume(afterMs, code) {
    await this.state.storage.put('pendingCode', code);
    await this.state.storage.setAlarm(Date.now() + afterMs);
    this.log(`alarm set for +${afterMs}ms`);
    return { scheduledIn: afterMs };
  }

  /** Cloudflare calls this with no request attached — the durable-timer path. */
  async alarm() {
    const code = await this.state.storage.get('pendingCode');
    if (!code) return;
    await this.state.storage.delete('pendingCode');
    const out = await this.runTurn(code);
    this.log(`alarm fired, ran pending turn -> ${JSON.stringify(out.step)}`);
    await this.state.storage.put('lastAlarmResult', out.step);
  }

  /** Simulate eviction: drop in-memory state without touching storage. */
  evict() {
    this.session = null;
    this.turnCount = undefined;
    this.log('evicted (in-memory state discarded)');
  }


  // -------------------------------------------------------------------------
  // Deploy-measurement probes. These exist to answer questions that local
  // workerd cannot: what the clocks do under real Workers, and where the real
  // CPU ceiling is. They are not part of the runtime design.
  // -------------------------------------------------------------------------

  /** Burn real CPU, reading every clock either side of it. */
  async timerProbe() {
    // The module-level wasm binding is per-isolate, so a probe that reads the
    // interpreter's clock has to initialise it first rather than relying on an
    // earlier request in the same isolate having done so.
    await this.ensureWasm();
    const jsPerf0 = performance.now();
    const jsDate0 = Date.now();
    const monty0 = AgentSession.clockNowMs();

    // Real work, not a sleep: sleeping would not distinguish a frozen clock
    // from an idle one.
    let acc = 0;
    for (let i = 0; i < 20_000_000; i++) acc += i % 7;

    const jsPerf1 = performance.now();
    const jsDate1 = Date.now();
    const monty1 = AgentSession.clockNowMs();

    return {
      burnResult: acc,
      jsPerformanceNow: { before: jsPerf0, after: jsPerf1, delta: jsPerf1 - jsPerf0 },
      jsDateNow: { before: jsDate0, after: jsDate1, delta: jsDate1 - jsDate0 },
      // The clock Monty's own max_duration is backed by, read inside the module.
      montyClockMs: { before: monty0, after: monty1, delta: monty1 - monty0 },
    };
  }

  /**
   * Arm Monty's own wall-clock limit and then exceed it in guest code.
   *
   * Experiment E predicted this limit cannot fire on Workers. If the prediction
   * holds, the guest loop runs to completion (or dies on Cloudflare's CPU limit)
   * and no interpreter-level timeout is ever raised.
   */
  async durationProbe(maxDurationMs, iterations) {
    await this.ensureWasm();
    const probe = new AgentSession('duration-probe.py', undefined, maxDurationMs);
    const started = Date.now();
    let step;
    let error = null;
    try {
      step = probe.start(`total = 0\nfor i in range(${iterations}):\n    total += i % 7\ntotal\n`);
    } catch (e) {
      error = String(e.message ?? e);
    }
    return {
      maxDurationMs,
      iterations,
      wallMs: Date.now() - started,
      step: step ?? null,
      error,
      // The question this probe exists for.
      montyTimeoutFired: /timeout|duration|time limit/i.test(error ?? JSON.stringify(step ?? {})),
    };
  }

  /** Guest CPU work of a given size — finds the effective per-request ceiling. */
  async cpuBurn(iterations) {
    await this.ensureWasm();
    const probe = new AgentSession('cpu-burn.py', undefined, undefined);
    const started = Date.now();
    try {
      const step = probe.start(`total = 0\nfor i in range(${iterations}):\n    total += i % 7\ntotal\n`);
      return { iterations, wallMs: Date.now() - started, step };
    } catch (e) {
      return { iterations, wallMs: Date.now() - started, error: String(e.message ?? e) };
    }
  }

  async fetch(request) {
    const url = new URL(request.url);
    const action = url.pathname.split('/').filter(Boolean).pop();
    try {
      switch (action) {
        case 'turn': {
          const { code } = await request.json();
          return json(await this.runTurn(code));
        }
        case 'resume': {
          const { value } = await request.json();
          return json(await this.resumeParked(value));
        }
        case 'schedule': {
          const { code, afterMs } = await request.json();
          return json(await this.scheduleResume(afterMs ?? 1000, code));
        }
        case 'alarm-result': {
          return json({ result: (await this.state.storage.get('lastAlarmResult')) ?? null });
        }
        case 'evict': {
          this.evict();
          return json({ evicted: true });
        }
        case 'timer-probe': {
          return json(await this.timerProbe());
        }
        case 'duration-probe': {
          const ms = Number(url.searchParams.get('ms') ?? 50);
          const iterations = Number(url.searchParams.get('n') ?? 3_000_000);
          return json(await this.durationProbe(ms, iterations));
        }
        case 'cpu-burn': {
          const iterations = Number(url.searchParams.get('n') ?? 1_000_000);
          return json(await this.cpuBurn(iterations));
        }
        case 'status': {
          // `dumpVersion()` is a static on the wasm class, so the module has to
          // be initialised even though this path never touches a session --
          // otherwise a cold object 500s, which is exactly what opening a
          // shared session link does.
          await this.ensureWasm();
          const rows = [...this.sql.exec('SELECT dump_version, turns, updated_at, length(bytes) AS n FROM snapshot WHERE id = 1')];
          const log = [...this.sql.exec('SELECT at, event FROM log ORDER BY seq')];
          return json({
            warm: this.session !== null,
            snapshot: rows[0] ?? null,
            dumpVersion: AgentSession.dumpVersion(),
            log,
          });
        }
        default:
          return json({ error: `unknown action ${action}` }, 404);
      }
    } catch (e) {
      return json({ error: String(e.message ?? e), stack: e.stack }, 500);
    }
  }
}

function json(body, status = 200) {
  return new Response(JSON.stringify(body, null, 2), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

// ---------------------------------------------------------------------------
// The version router. It only routes; it never holds an interpreter, because a
// session that crossed a worker boundary every turn would undo the warm path.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Bot check.
//
// Turnstile is verified **once per visitor**, not once per turn: this is a REPL,
// and re-solving a challenge on every code submission would make it unusable.
// A successful verification is exchanged for a short-lived HMAC-signed cookie,
// and that cookie is what the mutating actions actually check.
//
// The whole gate is inert unless `TURNSTILE_SECRET` is set, so local workerd
// and the integration suite keep working untouched.
// ---------------------------------------------------------------------------

const TURNSTILE_VERIFY = 'https://challenges.cloudflare.com/turnstile/v0/siteverify';
const GRANT_COOKIE = 'mlab_grant';
const GRANT_TTL_SECONDS = 1800;   // 30 minutes, then the widget is re-run
const MUTATING = new Set(['turn', 'resume', 'schedule']);

/// HMAC key for the grant cookie.
///
/// Domain-separated from the Turnstile secret by hashing it with a label, so the
/// value used to sign cookies is never the value sent to Cloudflare.
async function grantKey(env) {
  const raw = new TextEncoder().encode(`${env.TURNSTILE_SECRET}|grant-v1`);
  const bits = await crypto.subtle.digest('SHA-256', raw);
  return crypto.subtle.importKey('raw', bits, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign', 'verify']);
}

function b64url(bytes) {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function unb64url(s) {
  const pad = s.replace(/-/g, '+').replace(/_/g, '/');
  const bin = atob(pad + '='.repeat((4 - (pad.length % 4)) % 4));
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

/// `<expiry seconds>.<hmac>` — no identity in it, it only attests "a human got
/// past Turnstile recently".
async function issueGrant(env) {
  const payload = String(Math.floor(Date.now() / 1000) + GRANT_TTL_SECONDS);
  const key = await grantKey(env);
  const sig = await crypto.subtle.sign('HMAC', key, new TextEncoder().encode(payload));
  return `${payload}.${b64url(new Uint8Array(sig))}`;
}

async function grantValid(env, value) {
  if (!value || !value.includes('.')) return false;
  const [payload, sig] = value.split('.');
  let ok = false;
  try {
    const key = await grantKey(env);
    ok = await crypto.subtle.verify('HMAC', key, unb64url(sig), new TextEncoder().encode(payload));
  } catch {
    return false;   // malformed base64 etc.
  }
  if (!ok) return false;
  const exp = Number(payload);
  // Date.now() is frozen *within* a request but advances between them, which is
  // all an expiry check needs.
  return Number.isFinite(exp) && exp > Math.floor(Date.now() / 1000);
}

function cookie(request, name) {
  const raw = request.headers.get('cookie');
  if (!raw) return null;
  for (const part of raw.split(';')) {
    const [k, ...v] = part.trim().split('=');
    if (k === name) return v.join('=');
  }
  return null;
}

/// Returns a `Response` to reject, or `null` to allow.
async function verifyHuman(request, env, action) {
  if (!MUTATING.has(action)) return null;
  if (!env.TURNSTILE_SECRET) return null;   // gate not configured => open

  // A deliberate, documented bypass so the integration suite can run against a
  // gated deployment. Only active when LAB_TEST_KEY is set.
  if (env.LAB_TEST_KEY && request.headers.get('x-lab-test-key') === env.LAB_TEST_KEY) return null;

  if (await grantValid(env, cookie(request, GRANT_COOKIE))) return null;
  return json({ error: 'human-check-required' }, 401);
}

/// Exchange a Turnstile token for a grant cookie.
async function handleVerify(request, env, url) {
  if (!env.TURNSTILE_SECRET) return json({ ok: true, gate: 'disabled' });

  let token;
  try {
    ({ token } = await request.json());
  } catch {
    return json({ ok: false, error: 'expected {"token": "..."}' }, 400);
  }
  if (!token) return json({ ok: false, error: 'missing token' }, 400);

  const form = new FormData();
  form.append('secret', env.TURNSTILE_SECRET);
  form.append('response', token);
  const ip = request.headers.get('cf-connecting-ip');
  if (ip) form.append('remoteip', ip);

  const res = await fetch(TURNSTILE_VERIFY, { method: 'POST', body: form });
  const out = await res.json();
  if (!out.success) {
    return json({ ok: false, errors: out['error-codes'] ?? [] }, 403);
  }

  // `Secure` only over https, so the same code works against local workerd.
  const secure = url.protocol === 'https:' ? ' Secure;' : '';
  return new Response(JSON.stringify({ ok: true, ttl: GRANT_TTL_SECONDS }), {
    headers: {
      'content-type': 'application/json',
      'set-cookie': `${GRANT_COOKIE}=${await issueGrant(env)}; Path=/; Max-Age=${GRANT_TTL_SECONDS};${secure} HttpOnly; SameSite=Strict`,
    },
  });
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const parts = url.pathname.split('/').filter(Boolean);   // /run/:runId/:action

    // The demo UI. Static, no build step; see `demo.js`.
    if (parts.length === 0 || (parts.length === 1 && parts[0] === 'index.html')) {
      const siteKey = env.TURNSTILE_SITE_KEY ?? '';
      // The site key is public by design; it is injected rather than hardcoded
      // so the same bundle works gated or ungated.
      const html = DEMO_HTML.replace('__TURNSTILE_SITE_KEY__', siteKey);
      // The page is inline, so the policy is tight; Turnstile is the one
      // external origin, and only when it is actually configured.
      const ts = siteKey ? ' https://challenges.cloudflare.com' : '';
      return new Response(html, {
        headers: {
          'content-type': 'text/html; charset=utf-8',
          'content-security-policy':
            `default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'${ts}; ` +
            `connect-src 'self'${ts}; frame-src${ts || " 'none'"}; img-src data:`,
          'referrer-policy': 'no-referrer',
          'x-content-type-options': 'nosniff',
        },
      });
    }

    // Exchange a Turnstile token for the grant cookie.
    if (parts.length === 1 && parts[0] === 'verify') {
      return handleVerify(request, env, url);
    }

    if (parts[0] !== 'run' || parts.length < 3) {
      return json({ error: 'expected /run/:runId/:action' }, 400);
    }
    const runId = parts[1];

    const rejected = await verifyHuman(request, env, parts[2]);
    if (rejected) return rejected;
    // The name IS the address: no allocation, no lookup.
    const id = env.AGENT_RUN.idFromName(runId);
    return env.AGENT_RUN.get(id).fetch(request);
  },
};
