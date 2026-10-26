// An object literal against a union with primitive members: primitives are dropped, each
// written property checks against its types in the object members, and a discriminant
// never picks a primitive member.
const o = { a: 'x' };
declare const s: { a: string };
const k1: number | string | { a: number } | string[] = { a: 'x' };
const k2: number | { a: number } | { b: number } | string[] = { a: 'x' };
const k3: number | { b: number } | string[] = { b: 'x' };
const k4: number | { b: number } | string[] = { a: 'x', b: 1 };
const k5: number | { b: number } | number[] = { b: 'x' };
const k6: undefined | number | { b: number } | number[] = { b: 'x' };
const k7: null | { a: number } | string[] = { a: 'x' };
const k8: { a: number } | string[] = { a: 'x' };
const k9: number | string[] = { a: 'x' };
const m1: number | { b: number } | string[] = o;
const m2: number | { b: number } | { a: number } | string[] = o;
const m3: { b: number } | string[] = o;
const m4: string[] | number = o;
const m5: number | { a: number } | string[] = s;
const m6: number | { [k: string]: number } | string[] = { a: 'x' };
const m7: { a: number } | string[] | number = { a: 1, b: 2 };
type JsonN = number | { [k: string]: JsonN } | JsonN[];
const m8: JsonN = { a: { b: 'x' } };
type JsonM = number | { [k: string]: JsonM } | JsonM[];
const b1: JsonM | undefined = { b: 'x' };
const b2: number | { [k: string]: number } | number[] | undefined = { b: 'x' };
const b3: { [k: string]: number | { [k: string]: number } | number[] } | number = { a: { b: 'x' } };
const b4: { [k: string]: number | { [k: string]: number } | number[] | undefined } | number = { a: { b: 'x' } };
type Base<T> = { type?: string; required?: boolean; defaultsTo?: T };
type StrAttr = Base<string> & { type: 'string' };
type NumAttr = Base<number> & { type: 'number'; autoIncrement?: boolean };
type Attr = string | StrAttr | NumAttr;
const at: Attr = { type: 'string', autoIncrement: true, required: true };
// Aliased intersections sort by alias name.
type O2O = Base<any> & { model: string };
type Attr3 = string | StrAttr | NumAttr | O2O;
const at3: Attr3 = { type: 'string', autoIncrement: true, required: true };
export {}
