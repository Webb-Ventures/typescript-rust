// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "commonjs"}, "files": ["a.js", "b.js", "c.ts"]}
// @filename: a.js
// CommonJS exports keep literal types, narrow by assignment; `undefined` alone is TS7008.
exports.apply = undefined;
exports.apply = undefined;
function a() { }
exports.apply = a;
exports.apply()
exports.apply = 'ok'
var OK = exports.apply.toUpperCase()
exports.apply = 1
exports.k = void 0;
module.exports.m = 1;
module.exports.m = "s";
module.exports.m.toUpperCase();
// @filename: b.js
const { apply, m } = require('./a')
const result = apply.toFixed()
/** @type {number} */
const n = m;
// @filename: c.ts
// A tuple read past its end is `undefined`, compared with a later declaration.
declare var t: [number, string];
var t2 = t[2];
var t2: number | string;
