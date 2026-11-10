// A nested generic call whose contextual type is an outer call's type parameter infers
// what the outer call's contextual type implies for it (callChain.3).
declare function absorb<T>(): T;
declare const a: { m?<T>(obj: {x: T}): T } | undefined;
declare const b: { m?<T>(obj: {x: T}): T };
declare const c: { m<T>(obj: {x: T}): T } | undefined;
declare const d: { m<T>(obj: {x: T}): T };
declare function f<T>(obj: {x: T}): T;
export const n1: number | undefined = a?.m?.({x: absorb()});
export const n2: number | undefined = b.m?.({x: absorb()});
export const n3: number | undefined = c?.m({x: absorb()});
export const n4: number = d.m({x: absorb()});
export const n5: number = f({x: absorb()});
export const n6: number | undefined = f({x: absorb()});
