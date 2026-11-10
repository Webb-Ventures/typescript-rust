// A `boolean` parameter is never refined into an inferred predicate: `(x: boolean) => x`
// stays `(x: boolean) => boolean` (tsc: checkIfExpressionRefinesAnyParameter).
declare function take(f: (x: boolean) => boolean): void;
declare function takeP(f: (x: boolean) => x is true): void;
declare function g<T>(f: T): T;
const f1 = (x: boolean) => x;
const r = g((x: boolean) => x);
takeP((x: boolean) => x);
take((x: boolean) => x);
function f2(x: boolean) { return x; }
const f3 = (x: unknown) => typeof x === "string";
const f4 = (x: string | undefined) => x !== undefined;
const f5 = (x: boolean | undefined) => x;
const f6 = (x: true | false) => x;
export const t1: (x: boolean) => x is true = f1;
export const t2: (x: boolean) => x is true = f2;
export const t3: number = r;
export const t4: number = f3;
export const t5: number = f4;
export const t6: number = f5;
export const t7: number = f6;
export const t8: number = f1;
