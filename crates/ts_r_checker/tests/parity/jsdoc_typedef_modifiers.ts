// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "allowJs": true, "checkJs": true, "noEmit": true, "target": "ES2022"}, "files": ["a.js"]}
// @filename: a.js
// A JSDoc typedef's type literal may use `readonly`; the alias itself has no modifiers.
/** @typedef {{ readonly [key: string]: number }} R */
/** @typedef {{ readonly a: string }} S */
/** @type {R} */
const r = {};
/** @type {S} */
const s = { a: "" };
