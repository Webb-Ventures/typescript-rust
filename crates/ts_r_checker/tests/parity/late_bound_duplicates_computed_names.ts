// TS2300: members whose names collide through a computed key are all reported; a group with
// a plain name shows that name, one of computed keys only shows the first key as written.
const k = "key";
const one = "1";
const n = 1;
declare const u: unique symbol;
const fromFor = Symbol.for("x");
interface WithPlain {
    [k]: number;
    key: number;
}
interface OnlyComputed {
    [k]: number;
    [k]: number;
}
interface Numeric {
    [one]: number;
    [n]: string;
}
declare var literal: {
    [Symbol.iterator]: number;
    [Symbol.iterator]: number;
};
class Symbols {
    [u]: number = 1;
    [u]: string = "";
    [fromFor] = 1;
    [fromFor] = 2;
    static [k] = 1;
    static key = 2;
    [k] = 3;
}
// Methods (overloads) and accessor pairs are not duplicates.
class Methods {
    [k](): void;
    [k](x?: number) {}
    get [u]() { return 1; }
    set [u](v: number) {}
}

// TS1165/1166/1168/1169/1170: a computed name must be a literal or an entity name; its type
// is not checked here.
declare var str: string;
declare function make(): typeof u;
class Fields {
    [str] = 1;
    [str.length] = 2;
    [make()] = 3;
    [str + str] = 4;
    [(k)] = 5;
    [-1] = 6;
    [`tpl`] = 7;
    [make()](): void;
    [make()]() {}
}
declare class Ambient {
    [make()](): void;
}
interface Signatures {
    [str]: number;
    [make()]: number;
    [make()](): void;
}
type Literal = {
    [make()]: number;
};
export {};
