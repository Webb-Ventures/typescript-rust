// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "checkJs": true, "noEmit": true, "target": "ES2022", "module": "commonjs"}, "files": ["types.js", "script.js", "a.js", "b.js", "c.js", "d.js", "e.js", "f.ts"]}
// @filename: types.js
export const value = 1;
// @filename: script.js
var notAModule = 1;
// @filename: a.js
// `@import` tags that end early: the tokens end at the comment's end.
/**
 * @import
 */
/** @import a1 */
/** @import a2 from */
/**
 * @import { a3 }
 */
// @filename: b.js
/** @import b1 b2 */
/** @import b3 = require("./types") */
// @filename: c.js
// Type import attributes need exactly one key; a JavaScript script is not a module.
/** @import * as c1 from "./types" with { } */
/** @import * as c2 from "./types" with */
/** @import * as c3 from "./script" */
import * as c4 from "./script";
// @filename: d.js
// A JSDoc `@import` does not make a file a module: this one is a script.
/** @import { value } from "./types" */
var d1 = 1;
// @filename: e.js
import * as e1 from "./d";
// @filename: f.ts
import type * as f1 from "./types" with { };
import * as f2 from "./script";
export {};
