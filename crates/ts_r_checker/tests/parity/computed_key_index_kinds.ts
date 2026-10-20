// trpc `router.ts` (`{ [lazyMarker]: true }` with a branded `'lazyMarker' & { __brand }` key):
// a computed key whose type is a string-like intersection gives a string index signature;
// in object literals, number-like keys give a number index (joining a string one if any).
const lazyMarker = 'lazyMarker' as 'lazyMarker' & {
  __brand: 'lazyMarker';
};
export type Lazy<TAny> = (() => Promise<TAny>) & { [lazyMarker]: true };
declare const l: Lazy<number>;
const a: null = l[lazyMarker];
l[lazyMarker] = true as const;
declare const f: () => Promise<number>;
(f as Lazy<number>)[lazyMarker] = true as const;
type K = keyof { [lazyMarker]: true };
const k: null = null as any as K;
declare const s: string;
declare const n: number;
const o5 = { [n]: 1 };
const o5n: null = o5.x;
const o5i: null = o5[0];
const o4 = { a: 2, [n]: 1, b: "x", [s]: true };
const o4i: null = o4[0];
const o4s: null = o4["q"];
