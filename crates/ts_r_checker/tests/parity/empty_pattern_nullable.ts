// An empty binding pattern over a value that may be undefined or null is TS2532/TS2531;
// a pattern with elements reports its properties instead.
declare const v: void;
const {} = v;
const { a } = v;
declare const u: undefined;
const {} = u;
declare const n: string | undefined;
const {} = n;
const { length } = n;
declare const nl: null;
const {} = nl;
const [] = v;
declare const both: { x: 1 } | null | undefined;
const {} = both;
const {}: { x: 1 } | undefined = undefined;
