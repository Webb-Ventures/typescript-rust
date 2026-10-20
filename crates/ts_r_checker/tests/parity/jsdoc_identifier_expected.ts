// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext"}, "files": ["b.js"]}
// @filename: b.js
// `@implements` without a type is TS1003 after the tag name; a private name in
// `@property` is TS1003 at the `#` (not in `@param`).
class A { constructor() { this.x = 0; } }
/** @implements */
class B  {
}
/**
 * @implements
 */
class B2 {}
/**
 * @typedef A1
 * @type {object}
 * @property {string} #id
 * @property {number} ok
 */
/**
 * @param {string} #p
 * @param {number} q
 */
function f(q) {}
export { A, B, B2, f };
