// Arrays and open tuples cannot fill a target tuple's required elements (so recursive
// `T extends [infer L, ...infer R]` on `any[]` ends); optional elements print with
// `| undefined`; rest parameters of tuple type print expanded; intersections with a tuple
// make array literals tuples.
declare const sa: string[];
declare const aa: any[];
declare const t1: [string, ...string[]];
declare const t2: [string, number?, ...string[]];
declare const ro: readonly string[];
const x1: [string, ...string[]] = sa;
const x2: [any, ...any[]] = aa;
const x3: [string, string, ...string[]] = t1;
const x4: [string, number, ...string[]] = t2;
const x5: [...string[], string] = sa;
const x6: [string] = sa;
const x7: [string, ...string[]] = t1;
const x8: [...string[]] = sa;
const x9: readonly [string, ...string[]] = ro;
const x10: [string?, ...string[]] = sa;
type Rec<T extends unknown[]> = T extends [infer L, ...infer R] ? Rec<R> : 'done';
type X8 = Rec<any[]>;
const a8: X8 = null!;
const b8: null = a8;
type X2 = any[] extends [any, ...any[]] ? 1 : 2;
const c2: X2 = 1;

declare const p1: [string, number?];
declare const p2: [a: string, b?: number];
declare const p4: [string, (number | string)?];
declare const p5: [string?];
declare const p6: [string, any?];
function f(...args: [x: string, y?: boolean]) {}
declare let g: (...x: [number, string, ...boolean[]]) => void;
const y1: null = p1;
const y2: null = p2;
const y4: null = p4;
const y5: null = p5;
const y6: null = p6;
const y7: null = f;
const y8: null = g;

const yy: number[] & [number, ...number[]] = [1];
const xx: [number, ...number[]] = yy;
