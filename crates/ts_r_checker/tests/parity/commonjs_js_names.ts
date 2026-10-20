// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ESNext", "module": "commonjs", "allowJs": true, "checkJs": true, "noEmit": true, "types": []}, "files": ["a.js", "c.js", "e.js", "f.js", "g.js", "h.js", "i.js", "d.ts"]}
// @filename: a.js
module.exports = { x: 1 };
// @filename: c.js
const a = require("./a");
console.log(module, exports, a);
// @filename: e.js
import x from "./a";
const b = require("./a");
console.log(module, exports);
export { x, b };
// @filename: f.js
function g() { const z = require("./a"); return z; }
console.log(module);
// @filename: g.js
if (true) { module.exports.foo = 1; }
console.log(exports, typeof require);
// @filename: h.js
exports.a.b.c = 0;
// @filename: i.js
const q = 1;
console.log(module, exports, require, q);
// @filename: d.ts
const t = require("./a");
console.log(module);
export {};
