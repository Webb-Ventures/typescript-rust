// `o[k]` with a constant key of a literal union type narrows like a property.
type Opts = { wait?: { stdout?: RegExp; stderr?: RegExp } };
declare const opts: Opts;
const coll: { stdout?: string; stderr?: string } = {};
for (const stdio of ['stdout', 'stderr'] as const) {
  if (!opts.wait?.[stdio] || coll[stdio] === undefined)
    continue;
  coll[stdio] += "x";
  opts.wait[stdio].lastIndex = 0;
  const r = opts.wait[stdio].exec(coll[stdio]);
}
function f(o: { [k: string]: string | undefined }, key: string) {
  if (o[key]) o[key].length;
  let k2 = key;
  if (o[k2]) o[k2].length;
}
declare const w: { stdout?: RegExp; stderr?: RegExp };
function g(k: 'stdout' | 'stderr') {
  if (w[k]) w[k].lastIndex = 0;
  if (opts.wait?.[k]) opts.wait[k].lastIndex = 0;
  if (opts.wait?.[k]) opts.wait[k].exec("");
}
for (const k of ['stdout', 'stderr'] as const) {
  if (w[k]) w[k].lastIndex = 0;
}
