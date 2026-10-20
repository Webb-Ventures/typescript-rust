// Runs the vectors against a generated validator module.
// Usage: node driver.mjs <generated.mjs> <map.json>   (map: Cases key -> entry export name)
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import vectors from "./vectors.mjs";

const gen = await import(pathToFileURL(process.argv[2]).href);
const map = JSON.parse(readFileSync(process.argv[3], "utf8"));
const show = (x) => {
  try {
    return JSON.stringify(x, (_, v) => (typeof v === "bigint" ? `${v}n` : v === undefined ? "<undefined>" : v));
  } catch {
    return String(x);
  }
};
const fails = [];
let n = 0;
for (const [key, { ok = [], bad = [] }] of Object.entries(vectors)) {
  const v = gen[map[key]];
  if (!v) {
    fails.push(`${key}: no validator`);
    continue;
  }
  for (const x of ok) {
    n++;
    if (!v.is(x)) fails.push(`${key}: is() rejected ${show(x)}`);
    try {
      if (v.parse(x) !== x) fails.push(`${key}: parse() returned a different value`);
    } catch (e) {
      fails.push(`${key}: parse() threw for ${show(x)}: ${e.message}`);
    }
  }
  for (const [x, msg] of bad) {
    n++;
    if (v.is(x)) fails.push(`${key}: is() accepted ${show(x)}`);
    try {
      v.parse(x);
      fails.push(`${key}: parse() accepted ${show(x)}`);
    } catch (e) {
      if (!(e instanceof gen.ParseError)) fails.push(`${key}: threw ${e}`);
      else if (msg !== null && e.message !== msg) fails.push(`${key}: message\n    got:      ${e.message}\n    expected: ${msg}`);
    }
    const r = v.safeParse(x);
    if (r.ok) fails.push(`${key}: safeParse() accepted ${show(x)}`);
  }
}
console.log(JSON.stringify({ n, fails }));
