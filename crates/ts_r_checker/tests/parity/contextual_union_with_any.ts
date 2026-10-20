// An any member of a contextual union does not absorb the others (literals stay literal).
export {};
interface I {
    [index: number]: any;
    someOtherProperty: number;
}
interface J {
    [index: number]: number;
    someOtherProperty: number;
}
declare function f(args: ["a"] | I): void;
declare function g(args: ["a"] | J): void;
declare function h(args: ["a"] | { [index: number]: any }): void;
f(["a"]);
g(["a"]);
h(["a"]);
const v1: ["a"] | I = ["a"];
const v2 = ["a"] as ["a"] | I;
declare function k(args: { 0: any } | ["a"]): void;
k(["a"]);
