// Inferred type predicates (TypeScript 5.5): the one returned condition narrows through
// `x!` and `satisfies`, an `in` test drops `null` (and other primitives without the
// property), and with statements before the `return` the parameter starts from its type
// there (the `throw` removes `Date`).
type Foo = { foo: string };
type Bar = Foo & { bar: string };
declare const fooOrBar: Foo | Bar;
function isBarNonNull(x: Foo | Bar | null) {
  return ('bar' in x!);
}
if (isBarNonNull(fooOrBar)) {
  const t: Bar = fooOrBar;
}
function assertAndPredicate(x: string | number | Date) {
  if (x instanceof Date) {
    throw new Error();
  }
  return typeof x === 'string';
}
declare let snd: string | number | Date;
if (assertAndPredicate(snd)) {
  const t: string = snd;
}
function notOnlyIf(x: string | number) {
  if (x === 1) {
    return false;
  }
  return typeof x === 'string';
}
if (notOnlyIf(snd as string | number)) {
  const t: string = snd;
}
const arrTest: Array<number> = [1, 2, null, 3].filter(
  (x) => (x != null) satisfies boolean,
);
declare const v: string | { a: 1 } | null;
if ('length' in v!) {
  const n: never = v;
}
