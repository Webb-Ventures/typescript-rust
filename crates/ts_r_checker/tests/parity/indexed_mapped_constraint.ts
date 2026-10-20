// date-fns `buildMatchFn` (`const parsePatterns = (width && args.parsePatterns[width]) ||
// args.parsePatterns[args.defaultParseWidth]; Array.isArray(parsePatterns) ? findIndex(...)`):
// `{ [K in C]: X }[C]` relates as `X`, `(A & B)[K]` is constrained by `A[K] & B[K]`, and a
// generic `T[K]` read through its constraint narrows like that constraint.
type LocaleWidth = "narrow" | "short" | "any";
type C<V> = V extends string ? { a: 1 } : { b: 1 };
type M1<D extends LocaleWidth> = { [W in D]: string };
type M3<V, D extends LocaleWidth> = { [W in D]: C<V> };
type M4<V, D extends LocaleWidth> = { [W in LocaleWidth]?: C<V> };
type M5<V, D extends LocaleWidth> = M4<V, D> & M3<V, D>;
function f<V extends string | number, D extends LocaleWidth>(
  m1: M1<D>,
  m4: M4<V, D>,
  m5: M5<V, D>,
  d: D,
  z: { a: 1 } | { b: 1 } | undefined,
  c: C<V> | undefined,
) {
  const n1: number = m1[d];
  const x1 = m1[d];
  const n2: number = x1;
  if (Array.isArray(z)) {
    z.foo;
  }
  if (Array.isArray(c)) {
    c.foo;
  }
  const y4 = m4[d];
  if (Array.isArray(y4)) {
    y4.foo;
  }
  const y5 = m5[d];
  if (Array.isArray(y5)) {
    y5.length;
  }
}
// A deferred conditional type relates to a non-generic target through its constraint.
type Bit = 0 | 1;
type AndBit<A extends Bit, B extends Bit> = [A, B] extends [1, 1] ? 1 : 0;
type TestBit<A extends Bit, B extends Bit> = AndBit<A extends 1 ? 0 : 1, B>;
type Extends<A, B> = A extends B ? true : false;
type Not<T extends boolean> = T extends true ? false : true;
type X<T> = Not<Extends<T, number>>;
function g<V extends string | number>(c: C<V>) {
  const n: number = c;
}
export {};
