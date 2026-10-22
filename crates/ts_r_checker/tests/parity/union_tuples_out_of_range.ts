// An element no member of a union of tuples has is TS2339 naming the union.
type T2 = [boolean] | [string, number];
type T22 = T2[2];
type T21 = T2[1];
function f(t2: T2) {
  let [a, b, c] = t2;
  [a, b, c] = t2;
  const x = t2[2];
  const y = t2[1];
}
