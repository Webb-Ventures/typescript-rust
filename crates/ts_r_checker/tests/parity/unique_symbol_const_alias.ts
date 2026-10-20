// A const alias of a unique symbol (also through parentheses and `?:`) is a `symbol`;
// `Symbol()` itself and annotations keep the unique type.
const A = Symbol();
const B = A;
const x: typeof B = Symbol();
const C = (A);
const y: typeof C = Symbol();
const D = true ? A : A;
const z: typeof D = Symbol();
declare const F: unique symbol;
const G = F;
const v: typeof G = Symbol();
const I = Symbol.iterator;
const u: typeof I = Symbol();
const E: typeof A = A;
const e: typeof A = E;
