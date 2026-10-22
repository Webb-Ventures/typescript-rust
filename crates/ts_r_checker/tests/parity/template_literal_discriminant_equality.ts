// Equality narrowing compares a template literal discriminant of no type variable like a
// set of strings: `${string} is wrong!` is not `undefined`, but may be a matching string.
type Correct = { code: string; property: true; err: undefined };
type Err = { err: `${string} is wrong!` };
declare const example: Correct | Err;
if (example.err === undefined) {
  example.property;
}
if (example.err === "this is wrong!") {
  example.property;
}
type Err3 = { err: "x" };
declare const e3: Correct | Err3;
if (e3.err === undefined) {
  e3.property;
}
function g<T extends string>(v: { err: `${T}!` } | Correct) {
  if (v.err === undefined) {
    v.property;
  }
}
