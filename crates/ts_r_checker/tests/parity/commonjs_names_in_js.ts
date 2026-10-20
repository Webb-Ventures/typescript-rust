// @filename: tsconfig.json
{ "compilerOptions": { "allowJs": true, "checkJs": true, "noEmit": true, "strict": true, "target": "ES2020", "module": "commonjs", "lib": ["ES2020"] } }
// @filename: cjs.js
// A CommonJS indicator anywhere (here inside a function) makes `module` and `exports`
// defined; a one-argument `require(x)` call is an import. A bare `require` is still TS2591.
function load() { return require("./other"); }
if (typeof module !== "undefined") module.exports = load;
exports;
require;
// @filename: other.js
exports.y = 1;
// @filename: esm.js
// ECMAScript module syntax: `module` and `exports` are not defined, `require(x)` still is.
export const a = 1;
module.exports.b = 2;
exports.c = 3;
require("./other");
// @filename: script.js
// No indicator: `exports.a.b =`, `exports[k] =`, `exports.#p` and two-argument requires are not.
var k = "x";
exports.a.b = 1;
exports[k] = 1;
require("./other", 1);
module;
// @filename: script2.ts
// TypeScript files never define them.
module.exports = 1;
const r = require("./other");
