// Calling a getter's value is TS6234 (with or without a setter); other properties TS2349.
class C {
  get property(): number { return 1; }
  get y() { return ""; }
  set y(v) {}
  z = 1;
}
declare const c: C;
c.property();
c.y();
c.z();
