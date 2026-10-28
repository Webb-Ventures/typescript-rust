// A union in an array element, tuple element or interface type argument may name the alias
// being declared: the element is deferred, so the alias is no cycle (tsc 7.0.2).
type Value = ({ foo: string } | Value)[];
declare const v: Value;
const n1: number = v;
const n2: number = v[0];
type V2 = (string | V2)[];
declare const w: V2;
w.nope;
const e = w[0];
e.nope;
type V5 = Array<string | V5>;
declare const z: V5;
z.nope;
const n6: number = z;
type V7 = readonly (number | V7)[];
declare const r: V7;
const n7: string = r[0];
type T8 = [string, (number | T8)?];
declare const t8: T8;
const n8: string = t8[1];
const ok: V2 = ["a", ["b", []]];
const bad: V2 = ["a", [1]];
interface Box<T> { value: T }
type Box2 = Box<Box2 | number>;
const b20: Box2 = 42;
type I = (I & { x: 1 } | string)[];
declare const ii: I;
const ni: number = ii[0]!;
