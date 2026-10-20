// Literals whose contextual type is a const type parameter are in a const context:
// readonly tuples (mutable for a mutable array constraint), readonly properties, literal
// types. Const array literals combine as candidates. TS1277 on interfaces and aliases.
declare function one<const T extends readonly string[]>(values: T): T;
declare function two<const T extends readonly unknown[]>(values: T): T;
declare function three<const T>(values: T): T;
declare function four<const T extends string[]>(values: T): T;
declare function five<const T>(x: { a: T }): T;
declare function six<const T extends readonly unknown[]>(...args: T): T;
declare function seven<const T>(x: T | undefined): T;
declare function eight<const T extends { a: unknown }>(x: T): T;
const b1: number = one(['left', 'right']);
const b2 = one(['left', 'right']);
const n2: number = b2;
const c1: number = two(['left', 'right']);
const d1: number = three(['left', 'right']);
const d2: { x: number } = three({ a: 'left', b: [1, true], c: { d: `x` } });
const e1: number = four(['left', 'right']);
const f1: number = five({ a: ['x', 1] });
const g1: number = six('a', 1, ['b']);
const h1: number = seven(['a']);
const i1: number = eight({ a: ['a'] });
let v = 'q';
const j1: number = three([v, 1 + 1]);
declare function f4<const T>(obj: [T, T]): T;
const x41: number = f4([[1, "x"], [2, "y"]]);
interface I1<const T> { x: T }
type T1<const T> = T;
