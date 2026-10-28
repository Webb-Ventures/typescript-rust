// `new.target` is the constructor's class or the function's own type (arrows look
// through), its properties narrow like `this.x`, and outside those it is an error.
class Foo {
  constructor() {
    const a: never = new.target;
    const g = () => {
      const b: never = new.target;
    };
  }
}
function F() {
  const c: never = new.target;
}
const G = function () {
  const d: never = new.target;
};
class K {
  m() {
    const z = new.target;
  }
}
function foo(x: true) {}
function f() {
  if (new.target.marked === true) {
    foo(new.target.marked);
  }
}
f.marked = true;
