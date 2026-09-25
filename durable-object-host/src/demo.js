// A small demo UI for the runtime in `worker.js`.
//
// It is deliberately one self-contained page with no build step and no
// dependencies: the value of the demo is showing that a session survives, not
// showing off a front-end. Served by the router at `GET /`.
//
// The session ID is the run ID, which is the Durable Object's name — so pasting
// an ID back in reaches that exact object and continues that exact interpreter.
// Which also means **the ID is effectively a bearer token**: anyone holding it
// can read and continue the session. That is why new IDs are `crypto.randomUUID()`
// rather than anything short or sequential.
//
// The inline script below avoids template literals on purpose, since this whole
// file is one.

export const DEMO_HTML = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Monty durable session demo</title>
<style>
  :root {
    --bg: #0f1115; --panel: #171a21; --line: #272b36; --fg: #e6e8ee;
    --dim: #9aa3b2; --accent: #7aa2f7; --ok: #9ece6a; --warn: #e0af68; --err: #f7768e;
    --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--fg);
    font: 15px/1.55 ui-sans-serif, system-ui, -apple-system, sans-serif;
  }
  .wrap { max-width: 860px; margin: 0 auto; padding: 32px 20px 80px; }
  h1 { font-size: 20px; margin: 0 0 6px; letter-spacing: -0.01em; }
  .sub { color: var(--dim); font-size: 14px; margin: 0 0 28px; }
  .sub code { font-family: var(--mono); font-size: 12.5px; color: var(--fg); }
  .panel {
    background: var(--panel); border: 1px solid var(--line);
    border-radius: 10px; padding: 16px; margin-bottom: 16px;
  }
  label { display: block; font-size: 12px; text-transform: uppercase;
          letter-spacing: 0.06em; color: var(--dim); margin-bottom: 7px; }
  input[type=text], textarea {
    width: 100%; background: #0c0e13; color: var(--fg);
    border: 1px solid var(--line); border-radius: 7px; padding: 10px 12px;
    font-family: var(--mono); font-size: 13px; resize: vertical;
  }
  input[type=text]:focus, textarea:focus { outline: none; border-color: var(--accent); }
  textarea { min-height: 150px; line-height: 1.5; tab-size: 4; }
  .row { display: flex; gap: 10px; align-items: flex-end; }
  .row > div:first-child { flex: 1; }
  button {
    background: #222734; color: var(--fg); border: 1px solid var(--line);
    border-radius: 7px; padding: 10px 14px; font-size: 13px; cursor: pointer;
    font-family: inherit; white-space: nowrap;
  }
  button:hover:not(:disabled) { border-color: var(--accent); }
  button:disabled { opacity: 0.5; cursor: default; }
  button.primary { background: var(--accent); color: #0d1017; border-color: var(--accent); font-weight: 600; }
  .bar { display: flex; gap: 10px; align-items: center; margin-top: 12px; flex-wrap: wrap; }
  .hint { color: var(--dim); font-size: 12px; }
  .badges { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 12px; }
  .badge {
    font-family: var(--mono); font-size: 11.5px; padding: 3px 9px;
    border-radius: 999px; border: 1px solid var(--line); color: var(--dim);
  }
  .badge b { color: var(--fg); font-weight: 600; }
  .badge.new { border-color: var(--accent); color: var(--accent); }
  .badge.snapshot { border-color: var(--warn); color: var(--warn); }
  .badge.memory { border-color: var(--ok); color: var(--ok); }
  pre {
    margin: 0; font-family: var(--mono); font-size: 13px; white-space: pre-wrap;
    word-break: break-word;
  }
  .out { border-left: 2px solid var(--line); padding-left: 12px; }
  .out.ok { border-color: var(--ok); }
  .out.err { border-color: var(--err); }
  .out.parked { border-color: var(--warn); }
  .kind { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em;
          color: var(--dim); margin-bottom: 5px; }
  .examples { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 10px; }
  .examples button { padding: 5px 10px; font-size: 12px; font-family: var(--mono); }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  td { padding: 5px 0; vertical-align: top; }
  td:first-child { font-family: var(--mono); color: var(--accent); width: 190px; padding-right: 14px; }
  td:last-child { color: var(--dim); }
  details summary { cursor: pointer; color: var(--dim); font-size: 13px; }
  details[open] summary { margin-bottom: 10px; }
  .log { font-family: var(--mono); font-size: 12px; color: var(--dim); }
  .log div { padding: 2px 0; }
  .log .at { color: #5d6675; margin-right: 10px; }
  .tools { color: var(--dim); font-size: 12.5px; font-family: var(--mono); }
  a { color: var(--accent); }
</style>
</head>
<body>
<div class="wrap">

  <h1>Monty durable session</h1>
  <p class="sub">
    Python runs in a <a href="https://github.com/pydantic/monty/">Monty</a>
    interpreter compiled to wasm, inside a Cloudflare Durable Object named by
    your session ID. State persists between submissions — and survives the
    object hibernating, because the interpreter is snapshotted into that
    object's own SQLite. Paste a session ID back in later and the variables are
    still there.
  </p>

  <div class="panel">
    <div class="row">
      <div>
        <label for="sid">Session ID <span class="hint">— leave empty to start a new one</span></label>
        <input type="text" id="sid" placeholder="empty = new session" autocomplete="off" spellcheck="false">
      </div>
      <button id="fresh">New</button>
      <button id="copy">Copy link</button>
    </div>
    <div class="badges" id="badges"></div>
  </div>

  <div class="panel">
    <label for="code">Python</label>
    <div class="examples">
      <button data-ex="counter">counter</button>
      <button data-ex="func">define a function</button>
      <button data-ex="tool">call a tool</button>
      <button data-ex="cls">a class</button>
      <button data-ex="park">park on approval</button>
      <button data-ex="err">raise</button>
    </div>
    <textarea id="code" spellcheck="false"></textarea>
    <div class="bar">
      <button class="primary" id="run">Run turn</button>
      <span class="hint">&#8984;/Ctrl + Enter</span>
      <span style="flex:1"></span>
      <button id="refresh">Refresh status</button>
    </div>
  </div>

  <div class="panel" id="result" style="display:none"></div>

  <div class="panel" id="parked" style="display:none">
    <label for="approval">This run is parked — answer the tool call to resume it</label>
    <div class="row">
      <div><input type="text" id="approval" placeholder="a JSON value, e.g. &quot;yes-by-cfo&quot; or 42"></div>
      <button class="primary" id="resume">Resume</button>
    </div>
    <div class="bar"><span class="hint" id="parkedNote"></span></div>
  </div>

  <div class="panel">
    <details>
      <summary>Tools the guest can call</summary>
      <table>
        <tr><td>now()</td><td>today's date, ISO</td></tr>
        <tr><td>double(n)</td><td>n * 2</td></tr>
        <tr><td>remember(key, value)</td><td>writes to this object's SQLite</td></tr>
        <tr><td>recall(key)</td><td>reads it back</td></tr>
        <tr><td>fail()</td><td>always throws — becomes a catchable RuntimeError</td></tr>
        <tr><td>await_approval(x)</td><td>parks the run; snapshot taken while suspended</td></tr>
      </table>
      <p class="tools" style="margin-bottom:0">
        Undeclared <em>calls</em> suspend to the host every time, so the tool set
        is per-turn. An undeclared bare <em>name</em> is cached by the
        interpreter, so tools must be called, not referenced.
      </p>
    </details>
  </div>

  <div class="panel">
    <details>
      <summary>Session event log</summary>
      <div class="log" id="log"></div>
    </details>
  </div>

</div>

<script>
(function () {
  'use strict';

  var EXAMPLES = {
    counter: 'try:\\n    n = n + 1\\nexcept NameError:\\n    n = 1\\n\\nn',
    func: 'def tally(xs):\\n    return sum(xs)\\n\\ntally([1, 2, 3, 4])',
    tool: 'today = now()\\ndoubled = double(21)\\nremember("last_seen", today)\\n(today, doubled, recall("last_seen"))',
    cls: 'class Budget:\\n    def __init__(self, total):\\n        self.total = total\\n        self.spent = 0\\n\\n    def charge(self, n):\\n        self.spent += n\\n        return self.total - self.spent\\n\\nbudget = Budget(1000)\\nbudget.charge(120)',
    park: 'decision = await_approval("ship the release?")\\n"approved: " + decision',
    err: 'raise ValueError("this is catchable; the session survives")'
  };

  var $ = function (id) { return document.getElementById(id); };
  var sid = $('sid'), code = $('code'), badges = $('badges');
  var result = $('result'), parked = $('parked'), log = $('log');

  function sessionId(createIfEmpty) {
    var v = sid.value.trim();
    if (!v && createIfEmpty) {
      v = crypto.randomUUID();
      sid.value = v;
      syncHash();
    }
    return v;
  }

  function syncHash() {
    var v = sid.value.trim();
    if (v) { history.replaceState(null, '', '#' + encodeURIComponent(v)); }
    else { history.replaceState(null, '', location.pathname); }
  }

  function api(id, action, body) {
    return fetch('/run/' + encodeURIComponent(id) + '/' + action, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body || {})
    }).then(function (r) { return r.json(); });
  }

  function el(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) { n.className = cls; }
    if (text !== undefined) { n.textContent = text; }
    return n;
  }

  function badge(cls, label, value) {
    var b = el('span', 'badge ' + (cls || ''));
    b.appendChild(document.createTextNode(label + ' '));
    b.appendChild(el('b', null, String(value)));
    return b;
  }

  function showBadges(r) {
    badges.textContent = '';
    if (!r) { return; }
    if (r.loadedFrom) {
      var meaning = {
        'new': 'brand-new session',
        'memory': 'warm, interpreter still in memory',
        'snapshot': 'cold — rebuilt from SQLite'
      }[r.loadedFrom] || r.loadedFrom;
      var b = badge(r.loadedFrom, 'loaded from', r.loadedFrom);
      b.title = meaning;
      badges.appendChild(b);
    }
    if (typeof r.turns === 'number') { badges.appendChild(badge('', 'turns', r.turns)); }
    var n = r.snapshotBytes !== undefined ? r.snapshotBytes
          : (r.snapshot && r.snapshot.n !== undefined ? r.snapshot.n : undefined);
    if (n !== undefined && n !== null) { badges.appendChild(badge('', 'snapshot', n + ' B')); }
    if (r.dumpVersion !== undefined) { badges.appendChild(badge('', 'dump v', r.dumpVersion)); }
    if (r.warm !== undefined) { badges.appendChild(badge(r.warm ? 'memory' : 'snapshot', 'in memory', r.warm ? 'yes' : 'no')); }
  }

  function showResult(r) {
    result.style.display = '';
    result.textContent = '';
    parked.style.display = 'none';

    if (r.error && !r.step) {
      result.appendChild(el('div', 'kind', 'host error'));
      result.appendChild(el('pre', 'out err', r.error));
      return;
    }

    var step = r.step || {};
    if (step.kind === 'done') {
      result.appendChild(el('div', 'kind', 'value'));
      result.appendChild(el('pre', 'out ok', JSON.stringify(step.value, null, 2)));
    } else if (step.kind === 'error') {
      result.appendChild(el('div', 'kind', step.excType || 'error'));
      result.appendChild(el('pre', 'out err', step.error));
      result.appendChild(el('p', 'hint', 'The session survived this — your variables are intact.'));
    } else if (step.kind === 'parked') {
      result.appendChild(el('div', 'kind', 'parked mid-tool-call'));
      result.appendChild(el('pre', 'out parked',
        step.name + '(' + (step.args || []).map(function (a) { return JSON.stringify(a); }).join(', ') + ')'));
      parked.style.display = '';
      $('parkedNote').textContent =
        'Snapshot was written while suspended (' + r.snapshotBytes + ' B). ' +
        'You can close this tab, come back later, and resume — that is the durable-execution case.';
    } else {
      result.appendChild(el('div', 'kind', step.kind || 'result'));
      result.appendChild(el('pre', 'out', JSON.stringify(step, null, 2)));
    }

    if (r.toolCalls && r.toolCalls.length) {
      result.appendChild(el('div', 'kind', 'tool calls'));
      var box = el('pre', 'out');
      box.textContent = r.toolCalls.map(function (c) {
        var line = c.name + '(' + (c.args || []).map(function (a) { return JSON.stringify(a); }).join(', ') + ')';
        if (c.error) { return line + '  -> threw: ' + c.error; }
        if (c.outcome) { return line + '  -> ' + c.outcome; }
        return line + '  -> ' + JSON.stringify(c.result);
      }).join('\\n');
      result.appendChild(box);
    }
  }

  function showLog(rows) {
    log.textContent = '';
    if (!rows || !rows.length) { log.appendChild(el('div', null, 'nothing yet')); return; }
    rows.forEach(function (r) {
      var line = el('div');
      line.appendChild(el('span', 'at', (r.at || '').replace('T', ' ').replace('Z', '')));
      line.appendChild(document.createTextNode(r.event));
      log.appendChild(line);
    });
  }

  function busy(on) {
    $('run').disabled = on;
    $('resume').disabled = on;
    $('run').textContent = on ? 'Running…' : 'Run turn';
  }

  function run() {
    var src = code.value;
    if (!src.trim()) { return; }
    var id = sessionId(true);
    busy(true);
    api(id, 'turn', { code: src }).then(function (r) {
      showResult(r);
      showBadges(r);
      return api(id, 'status');
    }).then(function (s) {
      showLog(s.log);
    }).catch(function (e) {
      showResult({ error: String(e) });
    }).then(function () { busy(false); });
  }

  function resume() {
    var id = sessionId(false);
    if (!id) { return; }
    var raw = $('approval').value;
    var value;
    try { value = JSON.parse(raw); } catch (e) { value = raw; }
    busy(true);
    api(id, 'resume', { value: value }).then(function (r) {
      showResult(r);
      showBadges(r);
      return api(id, 'status');
    }).then(function (s) {
      showLog(s.log);
    }).catch(function (e) {
      showResult({ error: String(e) });
    }).then(function () { busy(false); });
  }

  function refresh() {
    var id = sessionId(false);
    if (!id) { showBadges(null); showLog([]); return; }
    api(id, 'status').then(function (s) {
      showBadges(s);
      showLog(s.log);
    });
  }

  $('run').addEventListener('click', run);
  $('resume').addEventListener('click', resume);
  $('refresh').addEventListener('click', refresh);
  $('fresh').addEventListener('click', function () {
    sid.value = '';
    sessionId(true);
    result.style.display = 'none';
    parked.style.display = 'none';
    showBadges(null);
    showLog([]);
    code.focus();
  });
  $('copy').addEventListener('click', function () {
    var id = sessionId(false);
    if (!id) { return; }
    navigator.clipboard.writeText(location.origin + location.pathname + '#' + encodeURIComponent(id));
    var b = $('copy'); b.textContent = 'Copied'; setTimeout(function () { b.textContent = 'Copy link'; }, 1200);
  });
  sid.addEventListener('change', function () { syncHash(); refresh(); });
  code.addEventListener('keydown', function (e) {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); run(); }
  });
  Array.prototype.forEach.call(document.querySelectorAll('[data-ex]'), function (b) {
    b.addEventListener('click', function () {
      code.value = EXAMPLES[b.getAttribute('data-ex')];
      code.focus();
    });
  });

  if (location.hash.length > 1) {
    sid.value = decodeURIComponent(location.hash.slice(1));
    refresh();
  }
  code.value = EXAMPLES.counter;
})();
</script>
</body>
</html>`;
