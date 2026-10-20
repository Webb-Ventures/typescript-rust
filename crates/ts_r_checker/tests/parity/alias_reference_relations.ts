// An alias-named reference relates as the plain reference: a missing property names the
// plain reference and leaves the head; other elaborations keep the alias in the head.
export {};
interface Box<T> { v: T; w: string }
interface Obj { hello: number; world: string }
interface Obj1 { hello: number }
interface Other { v: string; w: string }
type NB = Box<number>;
declare let nb: NB;
const x1: Box<string> = nb;
const x2: Obj = nb;
const x3: Obj1 = nb;
const x4: Other = nb;
declare let ob: Other;
const x5: NB = ob;
const x6: NB = { v: 1 };
type Arr = number[];
declare let ar: Arr;
const x7: string[] = ar;
const x8: Obj = ar;
type Tup = [number, string];
declare let tu: Tup;
const x9: [string, string] = tu;
const x10: Tup = [1, 2];
