// Destructuring assignments narrow their targets (array, object, nested,
// defaults, for-of heads); an array literal value of an array pattern is a tuple.
declare function load(): { tools: { run(): void }; other: number };
let tools: { run(): void } | undefined;
let other: number | undefined;
function f() {
  if (!tools)
    ({ tools } = load());
  tools.run();
}
function g() {
  if (!tools)
    ({ tools, other } = load() as ReturnType<typeof load>);
  tools.run();
  other.toFixed();
}
function h() {
  let a: string | undefined;
  let b: number | undefined;
  [a, b] = ["x", 1];
  a.length;
  b.toFixed();
  ({ x: a } = { x: "y" });
  a.length;
}
declare const src: { p?: string; q: { r?: number } };
function i() {
  let a: string | number | undefined;
  let b: number | undefined;
  ({ p: a = 1, q: { r: b = 2 } } = src);
  const n1: number = a;
  b.toFixed();
  ({ p: a } = src);
  a.length;
}
function j(pairs: [string, number][]) {
  let k: string | number | undefined;
  let v: string | number | undefined;
  for ([k, v] of pairs) {
    k.length;
    v.toFixed();
  }
  [k, v] = [v, k];
  k.toFixed();
}
function k2() {
  let a: string | number = "x";
  let rest: number[] | string;
  [a, ...rest] = [1, 2, 3];
  a.toFixed();
  rest.length;
}
