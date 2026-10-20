// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "checkJs": true, "noEmit": true, "strict": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler"}, "files": ["types.js", "cjs.js", "dup.js", "use.ts"]}
// @filename: types.js
// A top-level `@typedef` of a JavaScript ES module is exported.
export {};
/** @typedef {{ id: number }} Rec */
/** @callback Fn
 * @param {number} n
 * @returns {string}
 */
// @filename: cjs.js
// CommonJS modules keep a typedef local: it still clashes with a later class.
/** @typedef {number} Foo */
class Foo {}
module.exports = { n: 1 };
// @filename: dup.js
// A typedef named `default` against a default export: TS2300 under each name.
export default class C {}
/**
 * @typedef {string | number} default
 */
// @filename: use.ts
import type { Rec, Fn } from "./types.js";
const r: Rec = { id: "no" };
const f: Fn = (n) => n;
export { r, f };
