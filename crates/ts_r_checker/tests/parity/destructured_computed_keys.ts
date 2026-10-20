// Computed keys without a literal name in object binding patterns read through an index
// signature: TS2538 for `any` keys, TS2537 for `string` and `number` keys.
declare const k: any;
declare const ks: string;
declare const kn: number;
declare const r: { a: number };
declare const s: { [k: string]: number };
declare const n: { [k: number]: number };
declare const arr: number[];
let { [k]: y1 } = r;
let { [k]: y2 } = {};
let { [ks]: y3 } = { a: 1 };
let { [kn]: y4 } = r;
let { [kn]: y5 } = r as { a: number } | { [n: number]: string };
let { [k]: y6 } = s;
let { [k]: y7 } = n;
let { [k]: y8 } = arr;
let { [kn]: y9 } = "str";
for (let { [k]: y10 } of [r]) {}
function f({ [k]: y11 }: { a: number }) {}
function g({ [k]: y12 } = r) {}
const h = ({ [ks]: y13 = "" }) => y13;
let { [k]: y14 } = k;

// Element access keys: each failing member of a union is reported, a written bigint
// literal as `bigint`.
declare const u1: string | string[];
declare const u2: boolean | string[] | object;
declare const b: boolean;
declare const big: 5n;
arr[1n];
arr[b];
arr[u2];
arr[big];
r[1n];
export {};
