// Elements at or past a tuple's rest read the rest and every element after it; from the
// count of non-rest elements on they may be missing (noUncheckedIndexedAccess). Elements
// after the rest are not properties.
declare const array2: [...number[], number];
const el2: number = array2[1];
const x0: undefined = array2[0];
declare const array4: [number, ...number[], number];
const x4: undefined = array4[2];
const x5: undefined = array4[1];
type NE = [string, ...Array<string>];
declare const s: NE;
const [s0, s1, s2] = s;
const y0: undefined = s0;
const y1: undefined = s1;
declare const x: [number, ...string[], boolean];
const [a, b, c] = x;
const yb: undefined = b;
const yc: undefined = c;
const ib: undefined = x[1];
const ic: undefined = x[2];
declare const z: [number, string?, ...boolean[]];
const [z0, z1, z2] = z;
const yz1: undefined = z1;
const yz2: undefined = z2;
