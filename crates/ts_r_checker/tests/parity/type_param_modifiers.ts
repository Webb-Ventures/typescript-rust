// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler"}, "files": ["a.js", "b.ts"]}
// @filename: a.js
// Type-parameter modifiers: `const` on a typedef is TS1277, `in`/`out` on a function
// TS1274, any other modifier TS1273, at the keyword.
/**
 * @template const T
 * @typedef {[T]} X
 */

/**
 * @template const T
 */
export class C { }

/**
 * @template private T
 * @param {T} x
 * @returns {T}
 */
export function f(x) {
    return x;
}

/**
 * @template in T
 * @param {T} x
 */
export function g(x) {}

/**
 * @template in out T
 * @typedef {{ f: (x: T) => T }} Inv
 */

/**
 * @template out T
 */
export class D {}
// @filename: b.ts
export type T20<public T> = T;
export function f1<in T>(x: T) {}
export function f2<out T>() {}
export class K<in out T> { m<in U>() {} }
export interface I<const T> {}
export function f3<const T>(x: T) { return x; }
export type T21<readonly T> = T;
