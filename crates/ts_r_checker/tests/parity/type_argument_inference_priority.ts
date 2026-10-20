// Candidates inferred through type arguments of one generic type come first when no
// candidate is a supertype of the others.
export {};
interface Box<T> { v: T }
interface Sub extends Box<number> { w: 1 }
declare const ns: string;
declare const sn: string | number;
declare const nb: Box<number>;
declare const sub: Sub;
declare const na: number[];
declare const nra: readonly number[];
declare const np: Promise<number>;
declare const tup: [number];
declare const sv: { v: number };
declare function t1<T>(a: T, b: Box<T>): T;
declare function t2<T>(a: T, b: readonly T[]): T;
declare function t3<T>(a: T, b: Promise<T>): T;
declare function t4<T>(b: T[], a: T): T;
declare function t5<T>(a: T, b: [T]): T;
declare function t6<T>(a: T, b: { v: T }): T;
declare function t7<T>(x: T | Promise<T>): T;
declare const fp: false | Promise<true>;
t1(ns, nb);
t2(ns, nra);
t3(ns, np);
t4(na, ns);
t5(ns, tup);
t6(ns, sv);
t1(ns, sub);
const r1 = t1(sn, nb);
const c1: boolean = r1;
const r2 = t7(fp);
const c2: string = r2;
type Item<T> = { kind: 'a'; data: T } | { kind: 'b'; data: T[] };
declare function item<T>(a: T, b: Item<T>): T;
declare const it: Item<number>;
item(ns, it);
