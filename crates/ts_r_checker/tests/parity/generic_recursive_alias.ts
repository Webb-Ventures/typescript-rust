// Recursive generic aliases (tsc 7.0.2, parity defaults).
// A generic alias that names itself in a deferred position (an array element, a type
// argument of an interface) is a real recursive type, not `any`; a reference that is a
// generic alias's whole type keeps the alias when instantiated.
type RecArray<T> = Array<T | RecArray<T>>;
declare function flat<T>(a: RecArray<T>): Array<T>;
const r1: null = flat([1, ['a']]);
const r2: RecArray<string> = [1];
declare const q: RecArray<string>;
const r3: null = q[0];
type Tree<T> = { value: T } | Tree<T>[];
const t1: Tree<number> = [{ value: 1 }, [{ value: 'x' }]];
type A<T> = Array<T>;
type A3<T> = [T, T];
type A4<T> = Map<string, T>;
declare const a: A<string>, a3: A3<string>, a4: A4<string>;
const z: null = a;
const z3: null = a3;
const z4: null = a4;
type Grow<T> = Array<T | Grow<T[]>>;
declare const g: Grow<number>;
const g1: Grow<number> = [1, [[2]]];
