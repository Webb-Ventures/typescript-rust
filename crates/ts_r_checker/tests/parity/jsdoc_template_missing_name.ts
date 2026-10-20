// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "checkJs": true, "noEmit": true, "target": "ES2022", "module": "ESNext"}, "files": ["a.js"]}
// @filename: a.js
// `@template {T}` with no name is TS1069; where it is reported depends on what follows.
/** @template {T} */
export class A {}
/** @template {T}*/
export class B {}
/**
 * @template {string}
 */
export class C {}
/**
 * @template {number}
 * @template U
 * @param {U} u
 */
export function f(u) { return u; }
/** @template {T} U */
export class D {}
