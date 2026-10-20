// Validation throughput: ts-r parse<T> vs typia on the ten API types.
// usage: node bench.mjs <ours.js> <theirs.js> <data.js>
import { pathToFileURL } from "node:url";

const [oursPath, theirsPath, dataPath] = process.argv.slice(2);
const ours = (await import(pathToFileURL(oursPath))).validators;
const theirs = (await import(pathToFileURL(theirsPath))).validators;
const { data } = await import(pathToFileURL(dataPath));

const ok = (r) => (r && typeof r === "object" && "ok" in r ? r.ok : r.success);
// Same answers before timing anything.
for (const [name, { valid, invalid }] of Object.entries(data)) {
  for (const [who, v] of [["ts-r", ours[name]], ["typia", theirs[name]]]) {
    if (v.is(valid) !== true || v.is(invalid) !== false) throw new Error(`${who} ${name}: is`);
    if (v.parse(valid) !== valid) throw new Error(`${who} ${name}: parse valid`);
    if (ok(v.check(valid)) !== true || ok(v.check(invalid)) !== false) throw new Error(`${who} ${name}: check`);
    let threw = false;
    try {
      v.parse(invalid);
    } catch {
      threw = true;
    }
    if (!threw) throw new Error(`${who} ${name}: parse invalid`);
  }
}

const ops = {
  "is valid": (v, d) => v.is(d.valid),
  "is invalid": (v, d) => v.is(d.invalid),
  "parse valid": (v, d) => v.parse(d.valid),
  "parse invalid (throw)": (v, d) => {
    try {
      v.parse(d.invalid);
    } catch (e) {
      return e;
    }
  },
  "safeParse / validate valid": (v, d) => v.check(d.valid),
  "all errors (safeParse / validate)": (v, d) => v.check(d.invalid),
};

let sink = 0;
// ops/s of f over `ms` milliseconds (batches of 1000 calls).
function measure(f, ms) {
  let n = 0;
  const end = performance.now() + ms;
  const t0 = performance.now();
  while (performance.now() < end) {
    for (let i = 0; i < 1000; i++) if (f()) sink++;
    n += 1000;
  }
  return n / ((performance.now() - t0) / 1000);
}

const ROUNDS = 3, MS = 150;
const results = {};
for (const [op, run] of Object.entries(ops)) {
  results[op] = {};
  for (const [name, d] of Object.entries(data)) {
    const a = () => run(ours[name], d), b = () => run(theirs[name], d);
    measure(a, 50);
    measure(b, 50);
    const ra = [], rb = [];
    for (let r = 0; r < ROUNDS; r++) {
      ra.push(measure(a, MS));
      rb.push(measure(b, MS));
    }
    const med = (xs) => xs.sort((x, y) => x - y)[xs.length >> 1];
    results[op][name] = [med(ra), med(rb)];
  }
}

const fmt = (x) => (x >= 1e6 ? (x / 1e6).toFixed(1) + "M" : (x / 1e3).toFixed(0) + "k");
for (const [op, rows] of Object.entries(results)) {
  console.log(`\n${op}  (ops/s: ts-r | typia | ratio)`);
  let logSum = 0, aSum = 0, bSum = 0;
  for (const [name, [a, b]] of Object.entries(rows)) {
    console.log(`  ${name.padEnd(12)} ${fmt(a).padStart(7)} | ${fmt(b).padStart(7)} | ${(a / b).toFixed(2)}x`);
    logSum += Math.log(a / b);
    aSum += Math.log(a);
    bSum += Math.log(b);
  }
  const n = Object.keys(rows).length;
  console.log(`  geomean      ${fmt(Math.exp(aSum / n)).padStart(7)} | ${fmt(Math.exp(bSum / n)).padStart(7)} | ${Math.exp(logSum / n).toFixed(2)}x`);
}
console.log(`\n(sink ${sink > 0})`);
