// Inferring a rest type parameter after a fixed parameter from a source whose
// parameters are one tuple rest (a bound method) slices the tuple.
declare function wrap<A extends any[], R>(func: (location: number, ...args: A) => R): (...args: A) => R;
declare function f(location: number, title: string, fn?: Function): void;
const w = wrap(f);
w("a");
w(1);
class C {
  m(type: "a" | "b", location: number, title: string): void {}
  go() {
    const v = wrap(this.m.bind(this, "a"));
    v("x");
    v(1);
  }
}
declare function g(location: number, title: string): void;
const w2 = wrap(g.bind(null));
w2(2);
