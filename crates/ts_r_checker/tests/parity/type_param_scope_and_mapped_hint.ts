type K = number | string;
type T = {
    [K]: number;
};
type K4 = "x" | "y";
type T4 = {
    [K4]: number;
    k4: string;
};
function f0<T extends typeof a>(a: T) {
    return a;
}
declare function f1<Foo extends Bar, Bar>(Bar: any): void;
namespace N {
    export function g<U extends typeof g>(x: U) { return x; }
}
export {};
