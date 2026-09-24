// Experiment G — what does a guest blow-up do to a long-lived wasm instance?
//
// The instance holds ONE warm MontyRepl in a thread-local, as a Durable Object
// would. After each attempt to break it we ask two questions: did the host see
// a clean error or a wasm trap, and is the session still usable afterwards.
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const m = require('./pkg/crash_recovery_wasm.js');

const MiB = 1024 * 1024;
let instanceDead = false;

function classify(e) {
  if (e instanceof WebAssembly.RuntimeError) return `WASM TRAP: ${e.message}`;
  if (e instanceof RangeError) return `JS RangeError: ${e.message}`;
  if (typeof e === 'string') return `clean host error: ${e}`;
  return `${e?.constructor?.name ?? typeof e}: ${e?.message ?? e}`;
}

function attempt(label, fn) {
  console.log(`\n### ${label}`);
  const before = m.linear_memory_bytes();
  let result;
  try {
    result = { ok: true, value: fn() };
    console.log(`  outcome     : returned ${JSON.stringify(result.value)}`);
  } catch (e) {
    result = { ok: false, err: e };
    console.log(`  outcome     : threw -> ${classify(e)}`);
    if (e instanceof WebAssembly.RuntimeError) instanceDead = true;
  }
  const after = m.linear_memory_bytes();
  console.log(`  linear mem  : ${(before / MiB).toFixed(1)} MiB -> ${(after / MiB).toFixed(1)} MiB`);

  // The question that decides whether the pool's crash isolation is needed.
  let alive = null;
  try {
    alive = m.session_alive();
  } catch (e) {
    alive = `threw ${classify(e)}`;
  }
  console.log(`  session_alive: ${alive}`);
  return { ...result, alive };
}

console.log('=== baseline: build state in the warm session ===');
console.log('  ', m.feed("run_id = 'g-probe'"));
console.log('  ', m.feed("def helper(n):\n    return n * 3\n"));
console.log('  ', m.feed("kept = [i for i in range(100)]"));
console.log('   snapshot bytes:', m.snapshot_len());
console.log('   linear memory :', (m.linear_memory_bytes() / MiB).toFixed(1), 'MiB');

// 1. A limit Monty enforces itself.
attempt('guest recursion past Monty\'s own max_recursion_depth (default 1000)', () =>
  m.feed('def down(n):\n    return 0 if n == 0 else down(n - 1)\ndown(5000)'));

// 2. Raise Monty's limit so the *wasm stack* becomes the binding constraint.
//    This is the stack-overflow case Pydantic cite as a reason to use the pool.
for (const depth of [5000, 20000, 60000, 200000]) {
  if (instanceDead) break;
  m.reset_session_with_recursion_limit(depth + 1000);
  const r = attempt(`recursion ${depth} deep with Monty's limit raised above it`, () =>
    m.feed(`def down(n):\n    return 0 if n == 0 else down(n - 1)\ndown(${depth})`));
  if (!r.ok && r.err instanceof WebAssembly.RuntimeError) break;
}

// 3. Memory: try to exhaust wasm linear memory from guest code.
if (!instanceDead) {
  m.reset_session();
  m.feed("marker = 'pre-oom'");
  attempt('guest allocates a very large list (no max_memory set)', () =>
    m.feed('big = [0] * 200_000_000'));
}

if (!instanceDead) {
  attempt('guest builds a huge string', () =>
    m.feed("s = 'x' * 500_000_000"));
}

// 4. Does max_memory do anything without a tracking allocator?
if (!instanceDead) {
  m.reset_session_with_memory_limit(8 * MiB);
  attempt('max_memory(8 MiB) set, guest allocates well past it', () =>
    m.feed('big = [0] * 50_000_000'));
}

// 5. A guest loop that suspends forever, capped by the host.
if (!instanceDead) {
  m.reset_session();
  m.feed("count = 0");
  attempt('unbounded tool-calling loop, host caps at 50 calls', () =>
    m.feed_with_tool('total = 0\nwhile True:\n    total += tool()\ntotal', 1, 50));
}

// 6. Recovery: after everything above, can a fresh session be installed and used?
console.log('\n### recovery: install a fresh session and use it');
try {
  m.reset_session();
  console.log('  ', m.feed("recovered = 'yes'"));
  console.log('  ', m.feed('helper_gone = 1'));
  console.log('   feed after reset :', m.feed('recovered'));
  console.log('   snapshot bytes   :', m.snapshot_len());
  console.log('   linear memory    :', (m.linear_memory_bytes() / MiB).toFixed(1), 'MiB');
  console.log('   instance usable  : YES');
} catch (e) {
  console.log('   instance usable  : NO —', classify(e));
}

console.log('\n=== summary ===');
console.log(instanceDead
  ? '  A wasm trap occurred. Everything after it is suspect; the instance must be discarded.'
  : '  No wasm trap was triggered by any guest failure above; every failure surfaced as a\n  catchable host error and the instance stayed usable.');
