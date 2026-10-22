// The common supertype of inference candidates is computed without their `null` and
// `undefined` members, which are added back to it: `B` and `D | undefined` infer
// `B | undefined`, `"a" | undefined` and `"b"` infer `"a" | "b" | undefined`.
declare function equal<T>(a: T, b: T): T;
declare const v: never;
type B = { foo: string };
type D = { foo: string; bar: number };

const r1: B | undefined = equal(v as B, v as undefined | D);
const r2: B | undefined = equal(v as undefined | D, v as B);
const r3: "a" | "b" | undefined = equal(v as "a" | undefined, v as "b");
const r4: "a" | "b" | null | undefined = equal(v as "a" | null, v as "b" | undefined);
const r5: string | undefined = equal(v as string, v as (string & { tag: "foo" }) | undefined);
const n1: B = equal(v as B, v as undefined | D);
const n2: number = equal(v as "a" | undefined, v as "b");
