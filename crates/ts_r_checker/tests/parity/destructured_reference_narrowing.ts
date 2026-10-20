// An object binding of an unannotated variable initialized by a reference reads
// the narrowed property of that reference.
type Geo = { longitude: number; latitude: number; accuracy?: number };
function verify(g?: Geo) {
  if (!g) return;
  g.accuracy = g.accuracy || 0;
  const { longitude, accuracy } = g;
  if (accuracy < 0) throw new Error();
  return longitude;
}
function v2(o: { a?: string; b: { c?: number } }) {
  if (o.a && o.b.c) {
    const { a, b: { c } } = o;
    a.length;
    c.toFixed();
  }
  if (o.a) {
    let { a } = o;
    a.length;
  }
}
