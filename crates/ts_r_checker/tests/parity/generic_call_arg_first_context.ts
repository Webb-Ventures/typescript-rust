// A generic call argument resolves once, in the context it has while the outer call infers:
// with the outer type parameter still open, its enum literal argument widens.
enum Enum { A, B }
class Box<T> {
  constructor(val: T) { }
  convert(converter: { to: (v: T) => T; }) { }
}
interface Bx<T> { v: T }
function fn1<T>(arg: Box<T>) { return arg; }
function fn2<T>(arg: Bx<T>) { return arg; }
declare function mk<T>(x: T): Box<T>;
declare function mk2<T>(x: T): Bx<T>;
const r1 = fn1(new Box(Enum.A));
const r3 = fn1(mk(Enum.A));
const r5: Box<Enum> = mk(Enum.A);
const r6 = fn2(mk2(Enum.A));
const r7 = mk(Enum.A);
const t: [1, 2, 3, 4, 5] = [r1, r3, r5, r6, r7];
