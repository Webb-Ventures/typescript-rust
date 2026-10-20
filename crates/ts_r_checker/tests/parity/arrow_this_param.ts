// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext"}, "files": ["a.ts", "b.js"]}
// @filename: a.ts
// An arrow function's `this` parameter is TS2730 at the parameter.
export const f1 = (this: { n: number }, m: number) => m;
export const f2 = <T>(this: {n: number}, m: T) => m;
export const f3 = async (this: {n: number}, m: number) => m;
export function ok(this: { n: number }) { return this.n; }
// @filename: b.js
// `@this` on an arrow function is TS2730 at the tag name.
/**
 * @typedef {{fn(a: string): void}} T
 */

export class C {
    /**
     * @this {T}
     * @param {string} a
     */
    p = (a) => this.fn("" + a);
}
/** @this {T} */
export const g = () => 1;
/** @this {T} */
export const h = function () { this.fn(""); };
