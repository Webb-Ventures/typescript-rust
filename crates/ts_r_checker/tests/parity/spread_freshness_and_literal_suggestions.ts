// Excess properties and literal suggestions: an object literal with spreads is fresh, but only
// its own written properties are excess (also against a union's members); array literal
// elements elaborate in the target's context (a union with one array-like member elaborates
// against it); a string literal against a union of string literals suggests the closest one.
export {};
let x = { b: 1, extra: 2 };
let x1: { a: number; b: number } = { a: 1, ...x, z: 3 };
let x2: { a: number; b: number } = { a: 1, ...x };
let x3: { a: number; b: number } = { ...x, a: 1 };
let x4: { a: number; b: number } = { a: 1, ...{ b: 2, extra2: 3 } };
let x5: { a: number; b: number } = { z: 1, ...x, a: 1 };
let x6: { a: number; b: number } = { a: 1, b: 2, ...{} , y: 1 };
declare const g: { b: number; q: string } | undefined;
let x7: { a: number; b: number } = { a: 1, b: 2, ...g, w: 1 };
type Style = StyleBase | StyleArray;
interface StyleArray extends Array<Style> {}
interface StyleBase {
    foo: string;
}
const b0: Style = [{ foo: 'asdf', jj: 1 }];
const b2: StyleBase[][] = [[{ foo: 'asdf', jj: 1 }]];
const b3: Style[] = [[{ foo: 'asdf', jj: 1 }]];
type Disc = { kind: "hddvd" } | { kind: "bluray" };
declare function foo(x: Disc[]): void;
foo([{ kind: "bluray" }, { kind: "hdpvd" }]);
type T1 = "string" | "number" | "boolean";
type T2 = T1 & ("number" | "boolean");
type T3 = T1 & ("string" | "boolean");
const t1: T1 = "strong";
const t2: T2 = "strong";
const t3: T3 = "strong";
declare function f(x: T1): void;
f("strong");
const o: { k: T1 } = { k: "strong" };
const t4: "string" = "strong";
const t5: T1 | number = "strong";
declare let s: "strong";
const t6: T1 = s;
const t7: T1 = "Number";
type DU = { type: "a"; x: number } | { type: "b"; x: number } | { type: "a"; x: number; y?: string };
declare const ds: { type: "a" | "b"; x: number; z: boolean };
const du1: DU = { ...ds, x: 1 };
const du2: DU = { ...ds, x: 1, w: 2 };
