#!/usr/bin/env bash
# Deploy measurements for experiment F, against a *deployed* worker.
#
#   BASE=https://<worker>.workers.dev ./deploy-measure.sh
#
# The worker's own clocks are frozen under Workers (see NOTES.md), so every
# timing here is client-side wall clock. For server-side CPU, run
# `npx wrangler tail --format json` alongside this and read `cpuTime`.
#
# Step 5 is the one that matters: it waits out a real idle period to see whether
# Cloudflare hibernates the object on its own, rather than calling `evict`.
B=${BASE:?set BASE to the deployed worker URL}
RUN="measure-$(date +%s)"
t() { S=$(date +%s.%N); OUT=$(curl -s --max-time 120 "$@"); E=$(date +%s.%N); printf "%7.0fms  " $(echo "($E - $S) * 1000" | bc); echo "$OUT" | python3 -c "
import sys,json
try: d=json.load(sys.stdin)
except Exception: print(sys.stdin.read()[:80]); raise SystemExit
print('loadedFrom=' + str(d.get('loadedFrom')), 'value=' + str(d.get('step',{}).get('value'))[:30], 'snap=' + str(d.get('snapshotBytes')))
"; }

echo "run id: $RUN"
echo "--- 1. first request to a brand-new object (module init + session create) ---"
t -X POST -H 'content-type: application/json' -d '{"code":"state = {\"k\": \"v\"}\nlen(state)"}' $B/run/$RUN/turn
echo "--- 2. warm turn, session in memory ---"
t -X POST -H 'content-type: application/json' -d '{"code":"state[\"k\"]"}' $B/run/$RUN/turn
echo "--- 3. warm turn again ---"
t -X POST -H 'content-type: application/json' -d '{"code":"state[\"k\"]"}' $B/run/$RUN/turn
echo "--- 4. forced eviction, then a turn (cold wake from snapshot) ---"
curl -s -X POST $B/run/$RUN/evict > /dev/null
t -X POST -H 'content-type: application/json' -d '{"code":"state[\"k\"]"}' $B/run/$RUN/turn
echo "--- 5. idle for 150s, then a turn: did Cloudflare hibernate it on its own? ---"
sleep 150
t -X POST -H 'content-type: application/json' -d '{"code":"state[\"k\"]"}' $B/run/$RUN/turn
echo "--- 6. a second brand-new object, isolate already warm ---"
t -X POST -H 'content-type: application/json' -d '{"code":"state = 1\nstate"}' $B/run/${RUN}-b/turn
echo "DONE"
