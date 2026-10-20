// Context-sensitive functions nested in an overloaded call's object or array literal
// argument keep the parameter types of the first candidate whose full check reached them
// (its first pass reads them as any function); a later overload's literal without the
// property leaves them typed, not implicitly `any`.
interface Opts<K> {
  duration?: (group: K) => number;
}
declare function g<K>(sel: (v: number) => K): K;
declare function g<K>(sel: (v: number) => K, options: Opts<K>): K;
declare function g<K, E>(sel: (v: number) => K, element: (v: number) => E): K;

g((v) => v % 2, {
  duration: (group) => group.toFixed(),
});

declare function h(a: { cb?: (x: string) => number }): void;
declare function h(a: (y: number) => void): void;
h({ cb: (x) => x.length + "" });

declare function m(a: { cb: (x: number) => number }): 1;
declare function m(a: { cb: (x: string) => string }): 2;
export const r1 = m({ cb: (x) => "" + x });
export const r2 = m({ cb: (x) => x });

declare function n(a: { kind: "a"; cb: (x: number) => void }): 1;
declare function n(a: { kind: "b"; cb: (x: string) => void }): 2;
export const r3 = n({ kind: "b", cb: (x) => x.length });
export const r4 = n({ kind: "a", cb: (x) => x.toFixed() });

declare function p(a: [(x: number) => void]): 1;
declare function p(a: [(x: string) => void, number]): 2;
export const r5 = p([(x) => x.length, 1]);

declare function q<T>(a: { make: () => T; use: (t: T) => void }): T;
declare function q(a: number): number;
export const r6 = q({ make: () => 1, use: (t) => t.toFixed() });
