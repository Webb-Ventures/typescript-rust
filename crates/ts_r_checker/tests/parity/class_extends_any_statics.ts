// A class extending an `any` value: unknown members are `any` on both sides, known ones
// keep their types.
declare var Base: any;
class C extends Base {
  known = 1;
  static sknown = 2;
}
const c = new C();
c.known.length;
C.sknown.length;
c.unknown.length;
C.sunknown.length;
