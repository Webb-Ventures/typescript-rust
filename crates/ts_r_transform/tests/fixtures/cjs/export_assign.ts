// @target: es2022
// @module: commonjs
import fs = require("fs");
import type T = require("./t");
class C { f = fs; }
export = C;
