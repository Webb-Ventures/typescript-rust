// A readonly array or tuple before a postfix (`[]`, `?`, `[K]`) prints in parentheses; in
// an intersection or after `keyof` it does not.
declare const a: (readonly string[])[];
declare const b: (readonly [1, 2])[];
declare const c: [(readonly string[])?];
declare const f: readonly string[] & { x: 1 };
declare const h: ReadonlyArray<readonly number[]>;
declare function k<T extends readonly unknown[]>(x: T): (readonly [...T])[];
declare const g: keyof readonly string[];
export const n1: number = a;
export const n2: number = b;
export const n3: number = c;
export const n4: number = f;
export const n5: number = h;
export const n6: number = k([1]);
export const n7: number = g;
