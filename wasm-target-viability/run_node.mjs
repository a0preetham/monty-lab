// Harness: proves the wasm module runs Python, not merely that it links.
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const m = require('./pkg/monty_wasm_probe.js');

const cases = [
  "1 + 1",
  "sum(i*i for i in range(10))",
  "', '.join(sorted(['pear','apple','fig']))",
  "def fib(n):\n    return n if n < 2 else fib(n-1) + fib(n-2)\nfib(20)",
  "import math\nmath.sqrt(144.0)",
  "class P:\n    def __init__(self, x): self.x = x\n    def double(self): return self.x*2\nP(21).double()",
  "[k for k in {'a':1,'b':2}]",
];
let pass = 0, fail = 0;
for (const src of cases) {
  try {
    const out = m.eval_py(src);
    console.log(`  OK   ${JSON.stringify(src).slice(0,58).padEnd(60)} -> ${out}`);
    pass++;
  } catch (e) {
    console.log(`  FAIL ${JSON.stringify(src).slice(0,58).padEnd(60)} -> ${e}`);
    fail++;
  }
}
// error path must surface as a Python exception, not a trap
try {
  m.eval_py("raise ValueError('from guest')");
  console.log("  FAIL exception did not propagate"); fail++;
} catch (e) { console.log(`  OK   guest exception surfaced -> ${e}`); pass++; }

// entropy path (crypto.getRandomValues import)
try { console.log(`  OK   getrandom -> ${m.random_u128()}`); pass++; }
catch (e) { console.log(`  FAIL getrandom -> ${e}`); fail++; }

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
