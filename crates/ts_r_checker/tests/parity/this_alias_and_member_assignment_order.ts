// The object of an assigned member is evaluated before the value; a const alias of a
// property of `this` narrows `this`'s reference.
let x: string | boolean | number;
let obj: any;
x = true;
(x = "", obj).foo = (x = x.length);
export const n: number = x;
(x = "", obj)[x.length] = 1;
type Test = TestA | TestB;
interface TestA { type: 'a'; name: string; }
interface TestB { type: 'b'; value: number; }
export function t1(this: { test: Test }) {
  const t = (((((this).test)).type));
  if (t === "a") { return this.test.name; }
  return "";
}
