// trpc `mergeRouters(...routerList: [...TRouters])`: a generic rest parameter infers from the
// arguments in its positions as a tuple; a covariant inference that fits none of the
// contravariant ones gives way to them (`call(f, ...args)`).
interface R<Rec> {
  _def: { record: Rec };
}
type Merge<T extends R<any>[], Acc = {}> = T extends [infer H extends R<any>, ...infer Tl extends R<any>[]]
  ? Merge<Tl, H["_def"]["record"] & Acc>
  : Acc;
declare function merge<T extends R<any>[]>(...l: [...T]): Merge<T>;
declare const r1: R<{ foo: 1 }>;
declare const r2: R<{ bar: 2 }>;
const m = merge(r1, r2);
const mn: null = m;
declare function tup<T extends R<any>[]>(...l: [...T]): T;
const tn: null = tup(r1, r2);
declare function tup2<T extends R<any>[]>(...l: T): T;
const tn2: null = tup2(r1, r2);
declare function tup3<T extends unknown[]>(...l: [...T]): T;
const tn3: null = tup3(1, "a");
declare function tup4<T extends unknown[]>(...l: [string, ...T]): T;
const tn4: null = tup4("x", 1, true);
declare function call<A extends unknown[], R>(f: (...a: A) => R, ...args: A): R;
const c1: null = call((x: number, y: string) => x, 1, "a");
call((x: number) => x, "bad");
const arr = [1, 2];
const s1: null = tup3(...arr);
const s2: null = tup3(1, ...arr);
declare function lit<T extends string[]>(...a: T): T;
const l1: null = lit("a", "b");
declare function cs<T extends ((x: number) => void)[]>(...a: T): T;
const s4: null = cs((x) => {}, (y) => {});
declare function ro<T extends readonly unknown[]>(...a: T): T;
const s5: null = ro(1, "a");
export {};
