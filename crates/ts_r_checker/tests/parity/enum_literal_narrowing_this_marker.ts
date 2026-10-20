// Narrowing and this-type fixes: an enum member and a literal of its value are the same value
// for === narrowing; a ThisType marker in a union contextual type (an optional parameter)
// types this; a mixin constructor may take ...args: any.
export {};
enum E { A = "A", B = "B" }
declare let e: E;
if (e === "A") {
    const x: E.A = e;
} else {
    const y: E.B = e;
}
declare let u: { t: E.A; a: 1 } | { t: E.B; b: 1 };
if (u.t === "A") {
    u.a;
} else {
    u.b;
}
const z: E.A = "A";
const w = "A" === E.A;
type Opts<D> = { data: D; render(): unknown } & ThisType<Inst<D>>;
interface Inst<D> {
    get<K extends keyof D>(name: K): unknown;
}
declare function M1<D>(o?: Opts<D>): Inst<D>;
declare function M2<D>(o: Opts<D>): Inst<D>;
M1({ data: { msg: "" }, render() { return this.get("msg"); } });
M2({ data: { msg: "" }, render() { return this.get("msg"); } });
M1({ data: { msg: "" }, render() { const h = (x: unknown) => x; return h(this.get("msg")); } });
M2({ data: { msg: "" }, render() { const h = (x: unknown) => x; return h(this.get("msg")); } });
interface M { m(): void; }
class A { a() {} }
declare const X: typeof A & (new (...args: any) => M);
class B extends X {}
new B().m();
new B().a();
