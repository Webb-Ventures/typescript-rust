// @target: es2022
// @module: preserve
// `export =` (or `import x = require()`) under `module: preserve`: no `export {}` marker
// (esbuild oracle: TSExportEquals, where the marker made the bundler see an ES module).
import type { T } from "./t";
import fs = require("fs");
let x: T = fs;
export = [x, foo];
function foo() {}
