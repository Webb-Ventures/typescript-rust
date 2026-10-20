// Array destructuring (binding and assignment patterns, nested ones too), spreads and
// `yield*` need iterables (TS2488). `never` spreads are fine; `unknown` ones are not.
declare const u: unknown;
declare const a: any;
declare const n: number[] | null;
declare const o: { x: number };
declare const t: [number, [string]];
declare const nv: never;
var [a1] = u;
var [a2] = a;
var [a3] = n;
var [a4] = o;
var [, [a5]] = t;
var [, [[a6]]] = t;
let b1: number, b2: number;
[b1] = o;
[b1, [b2]] = [1, 2];
function f([x]: number) {}
function g([...r] = null) {}
declare function h2([y]?: [number]): void;
for (const [c1] of [1, 2]) {}
for ([b1] of [1, 2]) {}
const s1 = [...o];
const s2 = [...u];
const s3 = [...nv];
const [z] = nv;
function* gen() { yield* o; }
var [] = o;
const [d1] = "abc";
let [e1 = 1] = [undefined];
declare function h(...args: number[]): void;
h(...n);
declare const ni: number[] | ({ t: "a" } & { t: "b" });
let [el2] = ni;
function fn<T extends Array<unknown> | undefined>(arg: T) {
    if (typeof arg !== "undefined") {
        for (const p of arg) { }
        const m = [...arg];
    }
}
