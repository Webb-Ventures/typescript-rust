// An aliased condition narrows a constant reference: a const, an unassigned parameter or
// local let (not var), this, or a read-only property or tuple element of one.
type U = { kind: "foo"; foo: string } | { kind: "bar"; bar: number };
function f1(arg: U) {
  let obj = arg;
  const isFoo = obj.kind === "foo";
  if (isFoo) obj.foo;
}
function f2(arg: U) {
  var obj = arg;
  const isFoo = obj.kind === "foo";
  if (isFoo) obj.foo;
}
function f3(obj: { readonly x: string | number }) {
  const isString = typeof obj.x === "string";
  if (isString) { let s: string = obj.x; }
}
function f4(obj: { x: string | number }) {
  const isString = typeof obj.x === "string";
  if (isString) { let s: string = obj.x; }
}
function f5(obj: readonly [string | number]) {
  const isString = typeof obj[0] === "string";
  if (isString) { let s: string = obj[0]; }
}
function f6(outer: { readonly obj: U }) {
  const isFoo = outer.obj.kind === "foo";
  if (isFoo) outer.obj.foo;
}
function f7(outer: { obj: U }) {
  const isFoo = outer.obj.kind === "foo";
  if (isFoo) outer.obj.foo;
}
class C {
  constructor(readonly x: string | number, public y: string | number) {
    const a = typeof this.x === "string";
    const b = typeof this.y === "string";
    if (a && b) {
      let s: string = this.x;
      s = this.y;
    }
  }
}
function f8(obj: { readonly x: string | number }) {
  const isString = typeof obj.x === "string";
  obj = { x: 1 };
  if (isString) { let s: string = obj.x; }
}
function f9(arg: U) {
  let obj = arg;
  const isFoo = obj.kind === "foo";
  if (isFoo) obj.foo;
  obj = arg;
}
