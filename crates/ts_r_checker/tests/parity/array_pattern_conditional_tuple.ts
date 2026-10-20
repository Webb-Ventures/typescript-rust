// An array binding pattern types array literals reached through conditionals
// and logical operators as tuples.
declare const c: boolean;
declare const s: string;
const [a, b] = c ? [1, undefined] : [2, s];
const x: number = a;
const y: string = b;
const [d, e] = c ? [1, "x"] : [2, s] as const;
const z: number = e;
let [f, g] = c ? [1, "x"] : ([2, s]);
const w: number = g;
