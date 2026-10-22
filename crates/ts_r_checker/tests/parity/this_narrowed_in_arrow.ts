// A bare `this` in an arrow function keeps the enclosing function's narrowing; a function
// expression's own `this` and `this.p` in an arrow do not.
type W = { value: number; k: "w" };
type C = { name: string; k: "c" };
declare function isC(obj: any): obj is C;
declare function later(cb: () => void): void;
function f(this: C | W) {
  if (!isC(this)) return;
  later(() => {
    this.name;
  });
  later(function (this: C | W) {
    this.name;
  });
}
class K {
  p: C | W = { name: "", k: "c" };
  m() {
    if (this.p.k !== "c") return;
    later(() => {
      this.p.name;
    });
    const q = this.p;
    later(() => q.name);
  }
}
