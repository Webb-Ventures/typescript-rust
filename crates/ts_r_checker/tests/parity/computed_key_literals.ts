// Computed names print as their literal type; template expressions keep literal types
// in element access indexes and computed property names.
export {};
const c0 = "1";
const c1 = 1;
const c2 = "a-b";
const o1 = { [c0]: 1 };
const o2 = { [c1]: 1 };
const o3 = { ["1"]: 1 };
const o4 = { [c2]: 1 };
const o5 = { "1": 1 };
const o6 = { [`x${c0}`]: 1 };
let n1: number = o1;
let n2: number = o2;
let n3: number = o3;
let n4: number = o4;
let n5: number = o5;
let n6: number = o6;
declare const oo: { x1: number; y: string };
let q1: string = oo[`x${c0}`];
let q2: string = oo[(`x${c0}`)];
const o7 = { [(`x${c0}`)]: 1 };
let n7: number = o7;
