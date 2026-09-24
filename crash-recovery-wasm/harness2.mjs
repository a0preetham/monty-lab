// Does monty-alloc's soft limit convert the fatal trap into a catchable error?
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const m = require('./pkg/crash_recovery_wasm.js');
const MiB = 1024*1024;
const cls = e => e instanceof WebAssembly.RuntimeError ? `WASM TRAP: ${e.message}`
  : typeof e === 'string' ? `clean host error: ${e}` : `${e?.constructor?.name}: ${e?.message ?? e}`;

// The thread_local session is lazy, so force one allocation before reading
// LIVE_MEMORY — otherwise the allocator looks uninstalled when it is merely idle.
m.feed("warmup = 1");
console.log('allocator armed? live bytes =', (m.live_memory_bytes()/MiB).toFixed(3), 'MiB');

m.reset_session_with_memory_limit(64*MiB);
console.log("\n### max_memory(64 MiB) ARMED via monty-alloc; guest allocates ~1.6 GB");
m.feed("marker = 'pre-oom'");
try {
  const r = m.feed('big = [0] * 200_000_000');
  console.log('  outcome      : returned', r);
} catch (e) { console.log('  outcome      :', cls(e)); }
try { console.log('  session_alive:', m.session_alive()); } catch (e) { console.log('  session_alive: threw', cls(e)); }
try { console.log('  marker       :', m.feed('marker')); } catch (e) { console.log('  marker       : threw', cls(e)); }
try { console.log('  live memory  :', (m.live_memory_bytes()/MiB).toFixed(2), 'MiB'); } catch {}
try { console.log('  linear memory:', (m.linear_memory_bytes()/MiB).toFixed(1), 'MiB'); } catch {}

console.log("\n### a second, smaller over-allocation on the same session");
try { console.log('  outcome      :', m.feed("s = 'x' * 100_000_000")); }
catch (e) { console.log('  outcome      :', cls(e)); }
try { console.log('  session_alive:', m.session_alive()); } catch (e) { console.log('  session_alive: threw', cls(e)); }

console.log("\n### normal work continues afterwards?");
try {
  console.log('  ', m.feed("recovered = marker + ' -> survived'"));
  console.log('  ', m.feed('recovered'));
  console.log('   snapshot bytes:', m.snapshot_len());
  console.log('  VERDICT: instance survived a guest OOM');
} catch (e) { console.log('  VERDICT: instance did NOT survive —', cls(e)); }
