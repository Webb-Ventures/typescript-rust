// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "noEmit": true, "target": "ES2022", "module": "ESNext", "strict": true}, "files": ["a.js", "b.js", "c.js", "d.js"]}
// @filename: a.js
// Without `checkJs`, a JavaScript file still gets tsc's "plain JavaScript" errors.
let a = 1; let a = 2;
if ({} === {}) {}
function g() { await 1; }
class D { m() { this.#z; } }
export default 1;
export default 2;
// @filename: b.js
"use strict";
try {} catch (eval) {}
function h(arguments) {}
delete h;
label: label: ;
class K { m() { var arguments; } }
// @filename: c.js
// Other errors are not reported in plain JavaScript.
class C {} class C {}
missingName.y;
const q = 1; q = 2;
var n = 1; n();
if ({} == {}) {}
function* gen() { const yield = 1; }
// @filename: d.js
// @ts-nocheck
let d = 1; let d = 2;
