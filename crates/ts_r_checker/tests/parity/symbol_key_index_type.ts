// A symbol key an object lacks, without a symbol index signature, cannot index it
// (TS2538) when destructured, and in an element access when it has a string index.
const numIndexed: {[idx: number]: string} = null as any;
const strIndexed: {[idx: string]: string} = null as any;
const plain: { a: string } = null as any;
const symed = Symbol();
let symed2 = Symbol();
let {[symed]: prop6} = numIndexed;
let {[symed]: prop7} = strIndexed;
let {[symed2]: prop8} = numIndexed;
let {[symed2]: prop9} = strIndexed;
let {[symed2]: prop10} = plain;
let x1 = numIndexed[symed];
let x2 = strIndexed[symed2];
let k = true as boolean;
let {[k as any as object]: p11} = plain;
let p12: string;
({[symed]: p12} = strIndexed);
let z1 = numIndexed[symed];
let z2 = strIndexed[symed2];
let z3 = strIndexed[symed];
let z4 = plain[symed2];
let z5 = plain[symed];
let z6 = numIndexed[symed2];
let {[symed]: y1} = plain;
let {[symed2]: y2} = { a: 1 };
