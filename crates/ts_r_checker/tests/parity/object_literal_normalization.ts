// date-fns `renderFormatParse`: widening a union of object literals gives each member the
// properties only its siblings have, as optional `undefined` (nested literals too).
const xs = [
  { title: "a", n: 1 },
  { title: "b", n: 2, options: { x: true } },
  { title: "c", options: { y: true } },
];
xs.map(({ title, options = {} }) => title);
const ys = [{ a: 1 }, { b: "" }];
const y0: number = ys[0]!.b;
let o = Math.random() ? { a: 1 } : { b: "" };
o.a;
const nested = [{ p: { x: 1 } }, { p: { y: "" } }];
const nx: string = nested[0]!.p.x;
export default [{ a: 1 }, { b: "" }];
