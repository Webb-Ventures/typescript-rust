// @target: es2022
// @module: commonjs
// Imports are hoisted: references in earlier functions read the module variable.
function read() { return x + d + y(); }
import { x, y } from "./dep";
import d from "./def";
console.log(read());
