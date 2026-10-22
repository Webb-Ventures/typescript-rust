// A function argument's single returned literal stays a literal when the contextual
// return type is a type parameter that the call's own contextual type implies a literal
// type for: `const x: 0 | 1 | 2 = invoke(() => 1)` infers `T = 1`.
declare function invoke<T>(f: () => T): T;
declare function invoke2<T>(f: () => T, n: number): T;
const x1: 0 | 1 | 2 = invoke(() => 1);
const x2: 0 | 1 | 2 = invoke2(function () { return 1; }, 0);
type DooDad = "SOMETHING" | "ELSE";
const p1: Promise<DooDad> = Promise.resolve().then(() => { return "ELSE"; });
const p2: Promise<DooDad> = Promise.resolve().then(() => "ELSE");
const n1: 0 | 1 = invoke(() => 2);
const w = invoke(() => 1);
const n2: 1 = w;
