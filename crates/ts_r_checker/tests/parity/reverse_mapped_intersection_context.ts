// Contextual types through an intersection with a generic mapped type: the mapped part gives
// its template for the key, the index signatures of the other parts still apply, and an
// unconstrained type parameter beside it says nothing. The literal's functions are typed
// once, in the context the call's inference had (a type parameter with nothing inferred
// reads as its constraint once something is inferred); with overloads, in the context of
// the candidate that is chosen.
export {};
declare function cs<T>(reducers: { [K: string]: (state: string) => void } & { [K in keyof T]: object }): T;
const r1 = cs({ f(a) { const n: number = a; } });
const r2 = cs({ f(a) {}, g: 1 });
const c1: number = r1;

declare function m4<T>(obj: T & { [P in keyof T]: (s: string) => any }): void;
m4({ foo: s => { const n: number = s; } });

type Validate<T> = T & { [K in keyof T]: object };
declare function v<S, T extends Record<string, (state: S) => any>>(s: S, x: Validate<T>): void;
v(0, { foo: s => s + 1 });

declare function ov(x: number): void;
declare function ov<T>(reducers: { [K: string]: (state: string) => void } & { [K in keyof T]: object }): void;
ov({ f(a) { const n: number = a; } });
