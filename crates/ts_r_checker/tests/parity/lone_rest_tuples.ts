// A tuple of a lone rest element is its array (number[]); an array with a literal length
// is a tuple context.
export {};
declare const temp1: number[];
type X = [...number[]];
declare const x: X;
const n0: string = x;
const c1: [number, number, number] = [...temp1];
const c2: [number, ...string[]] = [...temp1];
type Y = readonly [...number[]];
declare const y: Y;
const n1: string = y;
declare const z: [...number[]];
const n2: string = z;
declare const z2: readonly [...number[]];
const n3: string = z2;
declare function f<T extends unknown[]>(...a: T): [...T];
const n4: string = f(...temp1);
type test1 = [...number[]];
type fixed1 = test1 & { length: 2 };
let var1: fixed1 = [0, 0];
let var2: fixed1 = [0, 0, 0];
type fixed2 = number[] & { length: 1 | 2 };
let var3: fixed2 = [0];
let var4: fixed2 = [];
