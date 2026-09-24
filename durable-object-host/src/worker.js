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
    await this.ensureWasm();
    if (this.session) {
      this.loadedFrom = 'memory';
      return this.session;
    }
    const rows = [...this.sql.exec('SELECT bytes, dump_version FROM snapshot WHERE id = 1')];
    if (rows.length) {
      const stored = rows[0].dump_version;
      const mine = AgentSession.dumpVersion();
      if (stored !== mine) {
        // Experiment D: this is where a multi-version build would dispatch to
        // the interpreter that can read `stored` instead of failing.
        throw new Error(`snapshot is dump v${stored}, this build reads v${mine}`);
      }
      this.session = AgentSession.restore(new Uint8Array(rows[0].bytes), undefined);
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
    return this.drive(session, session.start(code), []);
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
    const snapshotBytes = this.persist();
    this.log(`turn ${this.turnCount}: ${step.kind}`);

    return {
      step,
      toolCalls,
      loadedFrom: this.loadedFrom,
      snapshotBytes,
      suspended: session.suspended,
      turns: this.turnCount,
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
        case 'status': {
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

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const parts = url.pathname.split('/').filter(Boolean);   // /run/:runId/:action
    if (parts[0] !== 'run' || parts.length < 3) {
      return json({ error: 'expected /run/:runId/:action' }, 400);
    }
    const runId = parts[1];
    // The name IS the address: no allocation, no lookup.
    const id = env.AGENT_RUN.idFromName(runId);
    return env.AGENT_RUN.get(id).fetch(request);
  },
};
