"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.default = exports.baz = exports.bar = exports.foo = void 0;
// @module: commonjs
// @target: es2022
// A name exported twice is set to `void 0` once.
const foo = 1;
exports.foo = foo;
exports.bar = foo;
exports.baz = foo;
var a_1 = require("./a");
Object.defineProperty(exports, "default", { enumerable: true, get: function () { return a_1.x; } });
