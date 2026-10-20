// A generic source signature compared with a target signature infers its type arguments
// from the parameters, then from the return types (a union inference that breaks the
// constraint keeps the members that meet it), else `unknown`.
export {};
declare let a1: <T extends string>() => T | Promise<T>;
declare let b1: <T extends string>() => T;
declare let c1: <T extends string>() => Promise<T>;
a1 = b1;
a1 = c1;
declare let d1: <T extends string>() => T | number[];
d1 = b1;
declare let e1: <T extends string>() => boolean | number;
e1 = b1;
declare let f1: <T>() => T extends string ? 1 : 2;
declare let g1: <T>() => T extends string ? 1 : 2;
f1 = g1;
declare let h1: <S>() => S[];
declare let i1: <T>(x: T) => T;
i1 = h1;
declare let j1: <S>() => S;
declare let k1: <T>() => T;
k1 = j1;
declare let l1: <S, U>(x: U) => S;
declare let m1: <T, V>(x: T) => V;
m1 = l1;
declare let a2: <T>() => (T extends true ? true : false) & boolean;
declare let b2: <T>() => (T extends true ? true : false) & boolean;
a2 = b2;
declare let a3: <T>() => (T extends string ? 1 : 2) & { x: T };
declare let b3: <T>() => (T extends string ? 1 : 2) & { x: T };
a3 = b3;
type AB<U> = { a: U } | { b: U };
declare function foo<T, U>(obj: T & AB<U>): [T, U];
declare let ab: AB<string>;
const z: [AB<string>, string] = foo(ab);
