// A class, interface, array or tuple reference written as a type alias's body prints as
// the alias; a type parameter constrained to it elaborates the plain reference; a tuple
// that spreads a non-array type prints expanded.
export {};
declare class TableClass<S = any> { _field: S; }
type Table = TableClass;
type StrMap = Map<string, number>;
type Arr = Array<string>;
type Arr2 = string[];
type Tup = [string, number];
interface Box<T> { v: T }
type NB = Box<number>;
declare let m: StrMap;
declare let a: Arr;
declare let a2: Arr2;
declare let t: Tup;
declare let nb: NB;
declare let tb: Table;
const x1: number = m;
const x2: number = a;
const x3: number = a2;
const x4: number = t;
const x5: number = nb;
const x6: number = tb;
nb = { v: "s" };
m = 1;
const arr: NB[] = [nb];
const z: number = arr;
function g<T extends NB>(x: T) { const q: number = x; }
type Rec = Record<string, number>;
declare let r: Rec;
const x7: number = r;
type Numbers = number[];
type Unbounded = [...Numbers, boolean];
const data: Unbounded = [false, false];
type U2 = [...(string[]), boolean];
const d2: U2 = [false, false];
type RO = readonly string[];
declare let ro: RO;
const d3: string[] = ro;
const d4: number = ro;
