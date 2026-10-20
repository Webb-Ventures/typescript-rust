// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext"}, "files": ["b.js"]}
// @filename: b.js
// `@typedef Name` takes its first `@type` child as its type; each later `@type` is
// TS8033 just after its `}`.
/**
 * @typedef Name
 * @type {string}
 * @type {Oops}
 */
/**
 * @typedef Name2
 * @type {string}
 * @type {number} trailing words
 * @type {boolean}
 */
/**
 * @typedef {Object} Name3
 * @property {string} a
 * @type {number}
 */
/** @typedef Name4
 * @type {string} @type {number} */
/**
 * @typedef {Object} Name5
 * @type {{ a: { b: number } }}
 * @type {{ a: { b: number } }}
 */
/** @type {Name} */
export const x = 1;
/** @type {Name2} */
export const y = 1;
