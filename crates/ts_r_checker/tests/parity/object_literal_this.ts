// `this` in a method of an object literal without a contextual type is the literal's own
// widened type (under noImplicitThis), for methods and function expression values.
export {};
interface Foo { bar: boolean; }
declare function take(f: Foo): void;
var A = {
  p: 1,
  m() { take(this); const n: number = this; },
  n() { return this.p; },
  r() { return this.n() + this.p; },
};
const B = {
  inner: {
    x: "a",
    f() { return this.x; },
  },
  h() { return this.inner.x; },
};
var D = { lit: "x", f() { const s: "x" = this.lit; } };
var E = { f: function () { return this.g; }, g: 1 };
