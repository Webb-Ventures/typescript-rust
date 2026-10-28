// An array pattern over a non-tuple reads a numeric-named property before the element
// type, so noUncheckedIndexedAccess adds no `undefined` to it.
interface Pair extends Array<string> { 0: string; 1?: "b" }
declare const p: Pair;
const [a, b, c] = p;
const x: string = a;
const y: "b" = b;
const z: string = c;
declare const m: RegExpMatchArray;
const [whole, first] = m;
const w: string = whole;
const f: string = first;
