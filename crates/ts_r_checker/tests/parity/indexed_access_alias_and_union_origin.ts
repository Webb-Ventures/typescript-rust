// An indexed access written as an alias's type keeps the alias when it stays deferred or
// reads a union of keys, through instantiation too.
type Fields<T> = { [K in keyof T]: T[K] }[keyof T];
declare let fl: Fields<{ a: "x"; b: "y" }>;
fl = "z";
type G<T> = Fields<T>;
declare let g: G<{ a: "x"; b: "y" }>;
g = "z";
type One<T> = { [K in keyof T]: T[K] }["a" & keyof T];
declare let one: One<{ a: "x" | "w"; b: "y" }>;
one = "z";
function flat<T>(arr: T[], depth: number) {
    return arr.flat(depth);
}
let fr: number = flat([1], 1);

// Narrowing an aliased union comes back to a named union it was written with.
type O = { a: number; b: number };
type K = keyof O | "c";
function f2(k: K) {
    if (k !== "c") {
        k === "c";
    }
    switch (k) {
        case "c":
            k = "a";
    }
    k === "c";
}

// Type parameter defaults print in signatures.
declare function withDefaults<A, D extends number = 1, E = string>(a: A, d?: D): E;
let wd: number = withDefaults;
