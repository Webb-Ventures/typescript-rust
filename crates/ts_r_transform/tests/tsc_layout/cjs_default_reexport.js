"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.f2 = exports.f1 = exports.b = void 0;
// @module: commonjs
// @target: es2022
// A re-exported default import is assigned; `{ default as x }` is forwarded by a getter.
const t1_1 = __importDefault(require("./t1"));
exports.b = t1_1.default;
const t1_2 = __importDefault(require("./t1"));
exports.f1 = t1_2.default;
Object.defineProperty(exports, "f2", { enumerable: true, get: function () { return t1_2.default; } });
