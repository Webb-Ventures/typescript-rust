// An auto-typed let read before any assignment on the way is undefined.
declare function run(f: () => void): void;
declare function expectIt(x: unknown): void;
declare const c: boolean;

function f1() {
  let x;
  expectIt(x);
  x.foo;
}
function f2() {
  let x;
  if (c) x = 1;
  x.toFixed();
}
function f3() {
  let x;
  run(() => { x = 1; });
  x.toFixed();
  const y = x;
  y.foo;
}
function f4() {
  let x;
  run(() => x);
}
function f5() {
  let x;
  x = 1;
  run(() => x.toFixed());
}
function f6() {
  let x;
  run(() => x.toFixed());
  x = 1;
}
function f7() {
  let x = null;
  x.foo;
  const z = x;
}
