// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "noUnusedLocals": true, "noUnusedParameters": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler"}, "files": ["types.d.ts", "a.js", "b.js", "c.js"]}
// @filename: types.d.ts
export type Foo = string;
export type Bar = number;
// @filename: a.js
// noUnusedLocals/noUnusedParameters apply to JavaScript; `@import`s are value imports.
/** @import { Foo, Bar } from './types' */
/** @import * as T from './types' */

/** @typedef {{ a: number }} Unused */
/** @typedef {{ b: number }} Used */

/** @param {Used} u */
export function useIt(u) { return u; }

function helper() {}
/** @type {typeof helper} */
export const h = () => {};

function expando() {}
expando.prop = 1;

/**
 * @param {number} a
 * @param {number} b
 */
export function params(a, b) { return a; }

const local = 1;

export class K {
    /** @private */
    priv() {}
    #hidden = 1;
}
// @filename: b.js
// Top-level typedefs of a CommonJS module are not reported; nested ones are.
/** @typedef {number} Loc */
/** @callback Cb
 * @returns {void}
 */
function inner() {
  /** @typedef {string} Inner */
  return 1;
}
module.exports = { inner };
// @filename: c.js
// All unused `@template` parameters: TS6205 from before the first tag's `@`.
/**
 * @template T
 * @template V
 */
export class C1 {}
/**
 * @template T,V
 * @param {number} t
 */
export function f2(t) { return t; }
/**
 * @template T
 * @param {number} t
 */
export function f3(t) { return t; }
