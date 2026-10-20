// A type predicate narrows to a candidate that is a strict subtype of the declared type,
// even when the declared type is assignable to the candidate.
declare function isRecord(value: unknown): value is Record<string, unknown>;
declare function isEmpty(value: unknown): value is {};
declare function assertRecord(value: unknown): asserts value is Record<string, unknown>;
declare const o1: {};
if (isRecord(o1)) {
    const r: Record<string, unknown> = o1;
    o1["attr"];
    const n: number = o1;
}
declare const o2: {} | undefined;
if (isRecord(o2)) {
    o2["attr"];
}
declare const o3: Record<string, unknown>;
if (isEmpty(o3)) {
    o3["attr"];
}
declare const o4: {};
assertRecord(o4);
o4["attr"];

// A string key spelling a number selects the number index signature.
"abc"["1"];
"abc"[`1`];
declare const numbers: number[];
numbers["0"];
numbers[`0`];
const zero = `0`;
numbers[zero];
declare const byNumber: { [n: number]: string };
byNumber[`5`];
byNumber["1.5"];

// An empty enum has the numeric reverse mapping; a string enum does not.
enum Empty { }
const e1 = Empty[0];
enum Strings { A = "a" }
const e2 = Strings[0];

// `` f[`x`] = v `` declares an expando property like `f["x"] = v`.
function Foo() { }
Foo[`b`] = function () { };
type FooKeys = keyof typeof Foo;
const fk: FooKeys = "b";

export {};
