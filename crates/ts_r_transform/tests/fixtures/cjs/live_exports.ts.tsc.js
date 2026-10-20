"use strict";
var _a;
Object.defineProperty(exports, "__esModule", { value: true });
exports.z = exports.y = exports.x = void 0;
// @target: es2022
// @module: commonjs
// Locals exported by an export list stay in sync when they are reassigned.
let x = 0, z = 0;
exports.x = x;
exports.y = x;
exports.z = z;
exports.y = exports.x = x = 1;
exports.y = exports.x = x += 2;
exports.y = exports.x = (x++, x);
exports.y = exports.x = ++x;
exports.y = exports.x = (x--, x);
let w = (exports.y = exports.x = (_a = x++, x), _a);
w = (exports.y = exports.x = x = 5);
function f() { exports.y = exports.x = x = 9; }
exports.z = z = x;
