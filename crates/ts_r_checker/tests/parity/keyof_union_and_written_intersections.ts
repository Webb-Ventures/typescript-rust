// `keyof (T | U)` is `keyof T & keyof U`, printed without parentheses.
function f1<T, U>(k: keyof (T | U)) {
    k = "z";
}

// An intersection of two unions prints as written.
type A = { a: 1 }; type B = { b: 1 }; type C = { c: 1 }; type D = { d: 1 };
declare let ab: A & B;
let abcd: (A | B) & (C | D) = ab;

// Pieces of a distributed conditional left deferred are named by its alias.
type Kind = "x" | "y";
type Pick1<T extends Kind, A = { k: "x" } | { k: "y" }> = A extends { k: T } ? A : never;
function f2<T extends Kind>(p: Pick1<T>) {
    p = 1;
}
