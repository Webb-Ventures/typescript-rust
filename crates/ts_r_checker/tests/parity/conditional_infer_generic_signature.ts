// trpc `transformer.ts` (`ReturnType<typeof transformResultInner>` of a generic function):
// conditional types infer from a generic signature with its type parameters replaced by
// their constraints (`unknown` without one).
export {};
declare function inner<A extends string, B>(a: A, b: B): { a: A; b: B | undefined };
type R = ReturnType<typeof inner>;
const r: null = null as any as R;
type P = Parameters<typeof inner>;
const p: null = null as any as P;
declare function g2<T extends { x: number }>(t: T): T[];
const r2: null = null as any as ReturnType<typeof g2>;
