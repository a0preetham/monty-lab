// Experiment F — exercises the Durable Object host end to end against local
// workerd. Start the server first: `npx wrangler dev --local --port 8799`.
const BASE = process.env.BASE ?? 'http://localhost:8799';

let pass = 0, fail = 0;

async function call(runId, action, body) {
  const res = await fetch(`${BASE}/run/${runId}/${action}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body ?? {}),
  });
  const json = await res.json();
  if (!res.ok) throw new Error(`${action} -> ${res.status}: ${JSON.stringify(json)}`);
  return json;
}

function check(label, got, want) {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  console.log(`  [${ok ? 'ok' : 'FAIL'}] ${label}: ${JSON.stringify(got)}`);
  if (!ok) console.log(`         expected ${JSON.stringify(want)}`);
  ok ? pass++ : fail++;
  return ok;
}

function note(label, value) {
  console.log(`  ....  ${label}: ${JSON.stringify(value)}`);
}

const RUN = 'run_' + Math.random().toString(36).slice(2, 10);

console.log(`\n=== 1. warm path: consecutive turns on one object (${RUN}) ===`);
{
  let r = await call(RUN, 'turn', { code: "run_id = 'alpha'\nhistory = []\nlen(history)" });
  check('turn 1 value', r.step.value, 0);
  check('turn 1 came from', r.loadedFrom, 'new');

  r = await call(RUN, 'turn', { code: "def note(x):\n    history.append(x)\n    return len(history)\nnote('first')" });
  check('turn 2 value', r.step.value, 1);
  check('turn 2 is warm', r.loadedFrom, 'memory');
  note('snapshot bytes', r.snapshotBytes);
}

console.log('\n=== 2. async tools, including one backed by the object\'s own SQLite ===');
{
  let r = await call(RUN, 'turn', { code: "stamp = now()\ndoubled = double(21)\n(stamp, doubled)" });
  check('two async tools resolved', r.step.value[1], 42);
  note('tools called', r.toolCalls.map(t => t.name));

  r = await call(RUN, 'turn', { code: "remember('favourite', {'lang': 'python', 'n': 7})" });
  check('SQL-backed write tool', r.step.value, 'stored favourite');

  r = await call(RUN, 'turn', { code: "recall('favourite')['lang']" });
  check('SQL-backed read tool', r.step.value, 'python');
}

console.log('\n=== 3. a failing tool raises a catchable exception in the guest ===');
{
  const r = await call(RUN, 'turn', {
    code: "try:\n    fail()\n    out = 'no'\nexcept RuntimeError as e:\n    out = 'caught: ' + str(e)\nout",
  });
  check('guest caught the host failure', r.step.value.startsWith('caught:'), true);
  note('value', r.step.value);
}

console.log('\n=== 4. eviction and cold wake from the object\'s own storage ===');
{
  await call(RUN, 'evict');
  const r = await call(RUN, 'turn', { code: "(run_id, len(history), note('after-wake'))" });
  check('woke from snapshot, not memory', r.loadedFrom, 'snapshot');
  check('state survived eviction', r.step.value[0], 'alpha');
  check('function defined 5 turns ago still works', r.step.value[2], 2);
}

console.log('\n=== 5. park mid-tool-call, hibernate, resume days later ===');
{
  let r = await call(RUN, 'turn', {
    code: "decision = await_approval('spend 500')\nresult = 'approved: ' + decision if decision else 'denied'\nresult",
  });
  check('run parked mid-call', r.step.kind, 'parked');
  check('parked on the right tool', r.step.name, 'await_approval');
  check('snapshot taken while suspended', r.suspended, true);
  note('parked snapshot bytes', r.snapshotBytes);

  // The object is evicted while parked — as it would be over days.
  await call(RUN, 'evict');

  r = await call(RUN, 'resume', { value: 'yes-by-cfo' });
  check('resumed from a suspended snapshot', r.loadedFrom, 'snapshot');
  check('resumed to completion', r.step.value, 'approved: yes-by-cfo');

  r = await call(RUN, 'turn', { code: 'result' });
  check('resumed value persisted into session state', r.step.value, 'approved: yes-by-cfo');
}

console.log('\n=== 6. alarm-driven resumption, with no inbound request ===');
{
  await call(RUN, 'schedule', { code: "woken_by_alarm = note('by-alarm')\nwoken_by_alarm", afterMs: 500 });
  await new Promise(r => setTimeout(r, 2500));
  const r = await call(RUN, 'alarm-result');
  check('alarm ran the pending turn', r.result?.kind, 'done');
  note('alarm result', r.result);
}

console.log('\n=== 7. run IDs are separate objects ===');
{
  const other = 'run_' + Math.random().toString(36).slice(2, 10);
  const r = await call(other, 'turn', { code: "try:\n    run_id\nexcept NameError:\n    out = 'isolated'\nout" });
  check('a different run ID sees none of the first run\'s state', r.step.value, 'isolated');
  check('and starts fresh', r.loadedFrom, 'new');
}

console.log('\n=== 8. guest OOM inside the DO (experiment G, in situ) ===');
{
  // The guest CANNOT catch this: Monty raises resource errors uncatchably, so
  // `except MemoryError` does not run. The host sees the error instead, and the
  // object stays alive — which is the property that actually matters.
  const r = await call(RUN, 'turn', {
    code: "try:\n    big = [0] * 200_000_000\n    out = 'allocated'\nexcept MemoryError:\n    out = 'caught in guest'\nout",
  });
  check('OOM surfaces to the host, not the guest', r.step.kind, 'error');
  check('and is typed', r.step.excType, 'MemoryError');
  note('error', r.step.error);
  const after = await call(RUN, 'turn', { code: "(run_id, len(history))" });
  check('object still usable after guest OOM', after.step.value[0], 'alpha');
}

console.log('\n=== object status ===');
{
  const s = await call(RUN, 'status');
  note('dump version', s.dumpVersion);
  note('snapshot row', s.snapshot);
  console.log('  event log:');
  for (const e of s.log) console.log(`    ${e.at}  ${e.event}`);
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
