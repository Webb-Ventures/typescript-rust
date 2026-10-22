// Inference from a callback to a generic callback parameter sees the type parameters of
// either signature as their base constraints (`unknown` without one; `any` inside a
// circular one): the callback's type parameters never escape into the call's.
declare function f1<T>(cb: <S>(x: S) => T): T;
declare function f2<T>(cb: <S extends number>(x: S) => T): T;
declare function f3<T>(cb: <S extends Array<S>>(x: S) => T): T;
const x1 = f1((x) => x);
const x2 = f2((x) => x);
const x3 = f3((x) => x);
declare const s: <R>(go: <S>(ops: { init(): S }) => R) => R;
const x4 = s((a) => a.init());
const n1: number = x1;
const n2: string = x2;
const n3: number = x3;
const n4: number = x4;
const n5: string = f1(<U,>(x: U) => 1);
