// Relating to the members of an intersection target defers nested excess property and weak
// type checks to a check of the source's properties against the combined properties.
export {};
declare let w1: { a: { x: string }; c: number };
let t1: { a: { x: number } } & { c: number } = w1;
declare let w2: { a: { y: string } };
let t2: { a?: { x?: number } } & { c?: string } = w2;
declare let w3: { a: { y: string }; c: string };
let t3: { a: { x?: number } } & { c: string } = w3;
declare let w4: { a: string; c: string };
let t4: { a: number } & { c: string } = w4;
declare let w5: { c: string };
let t5: { a: number } & { c: string } = w5;
declare let w8: { x: { c: string } };
let t8: { x: { a?: string } } & { x: { b?: string } } = w8;
declare let w9: { x: { b: string } };
let t9: { x: { a?: string } } & { x: { b?: string } } = w9;
declare let x1: { __typename?: "TypeTwo" } & { a: boolean };
let y1: { __typename?: "TypeOne" } & { a: boolean } = x1;
type T1 = { primary: { __typename?: "Feature" } & { colors: { light: number; dark: number } } };
type T2 = { primary: { __typename?: "Feature" } & { colors: { light: number } } };
const response: T1 & T2 = { primary: { colors: { light: 1, dark: 3 } } };
const bad: T1 & T2 = { primary: { colors: { light: 1, dark: 3, dim: 2 } } };
