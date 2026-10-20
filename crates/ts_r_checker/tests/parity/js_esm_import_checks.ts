// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "checkJs": true, "noEmit": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler"}, "files": ["lib.js", "script.js", "a.js"]}
// @filename: lib.js
export const one = 1;
// @filename: script.js
var notAModule = 1;
// @filename: a.js
// JavaScript ES modules get the same import checks as TypeScript.
import def from "./lib.js";
import { two } from "./lib.js";
import { x } from "./script.js";
export { def, two, x };
