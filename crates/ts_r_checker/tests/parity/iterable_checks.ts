// Values that are iterated (for-of, spreads, `yield*`) or destructured by an array pattern
// must be iterable (TS2488, TS2504), reported at the operand or the pattern.
export {};
declare const n: never;
declare const num: number;
declare const nu: number[] | null;
declare const o: { 0: string; 1: boolean };
declare const z: unknown;
declare function g(...args: any): any;
const iter = {
    *[Symbol.iterator](_: number) {
        yield 0;
    }
};
for (const x of n) { }
for (const q of nu) { }
for (const q of z) { }
const [a1] = n;
const [a2] = num;
const [a3] = nu;
const [a4, a5] = o;
let b1: any, b2: any;
[b1] = num;
const s1 = [...num];
const s2 = [...n];
const s3 = [...z];
g(...num);
g(...n);
for (const _ of iter);
function* gen() {
    yield* num;
    yield* iter;
}
async function* agen() {
    yield* num;
    yield* [1];
}
function f1([...r] = null) { }
function f2([x] = {}) { }
function f3([x]: [number] | undefined = [1]) { }
for (const [p, q] of [1, 2]) { }
for ([b1, b2] of [1, 2]) { }
const [[d1]] = [1];
declare const u: string | number;
const [c1] = u;
const [c2] = "ab";
function tp<T extends number[], U, V extends number[] | undefined>(x: T, y: U, v: V) {
    const a = [...x];
    const [d] = y;
    if (typeof v !== "undefined") {
        const e = [...v];
    }
}
declare const conflict: { a: "foo" } & { a: "bar" };
const [el1] = conflict;
declare const mixed: number[] | ({ t: "a" } & { t: "b" });
const [el2] = mixed;
