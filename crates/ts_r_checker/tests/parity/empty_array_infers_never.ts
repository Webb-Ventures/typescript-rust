// An empty array literal infers `never` for a call's type parameter, and a candidate
// inferred from inside another type (type argument, property, signature) keeps literal
// candidates unwidened.
declare class Box<T> {
  f(t: T): T;
}
declare function from<T>(v: T[]): Box<T>;
export const a1: string = from([]);
declare function first<T>(v: T[]): T;
export const a2: number = first([]);
declare function narrow<T extends string>(v: T[]): T;
export const a3: number = narrow([]);
declare function both<T>(a: T[], b: T): Box<T>;
export const a4: string = both([], 1);
export const a5: string = both([2], 1);
declare function boxed<T>(b: T, a: Box<T>): Box<T>;
declare const bn: Box<never>;
declare const b2: Box<2>;
declare const bnum: Box<number>;
export const a6: string = boxed(1, bn);
export const a7: string = boxed(1, b2);
export const a8: string = boxed(1, bnum);
declare function prop<T>(a: { x: T }, b: T): Box<T>;
declare const ox: { x: 2 };
export const a9: string = prop(ox, 1);
export const a10: string = prop({ x: 2 }, 1);
declare function inter<T>(a: T & {}, b: T): Box<T>;
export const a11: string = inter(2, 1);
declare function pair<T>(a: T, b: T): Box<T>;
declare const nv: never;
export const a12: string = pair(1, nv);
export const a13: number = Array.from([]);
export const a14: number = new Set([]);
export const a15: number = new Map([]);
