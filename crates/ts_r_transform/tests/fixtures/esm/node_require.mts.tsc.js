import { createRequire as _createRequire } from "module";
const __require = _createRequire(import.meta.url);
// @target: es2022
// @module: nodenext
const fs = __require("fs");
const path = __require("path");
const os = __require("os");
export { os };
console.log(fs, path);
