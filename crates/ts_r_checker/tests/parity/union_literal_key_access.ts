// A union of literal keys that each name a property reads those properties, without the
// `undefined` noUncheckedIndexedAccess adds for index signatures.
declare function controllable<T>(): { readonly observable: T };
const closings = [controllable<void>(), controllable<void>()] as const;
const c3 = [{ a: 1 }, { a: 2 }] as const;
declare const c5: [{ a: 1 }, { b: 2 }];
declare const i: 0 | 1;
closings[i].observable;
c3[i].a;
export const n: string = c5[i];
declare const o: { x: number; y?: string; [k: string]: number | string | undefined };
declare const k: "x" | "y";
export const v: boolean = o[k];
declare const k2: "x" | "z";
export const v2: boolean = o[k2];
declare const arr: number[];
export const v3: boolean = arr[i];
declare function bt<O>(openings: O[], sel: (o: O) => unknown): O;
declare const os: (0 | 1)[];
bt(os, (index) => closings[index].observable);
