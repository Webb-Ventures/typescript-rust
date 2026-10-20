"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
// @target: es2022
// @module: commonjs
// Imports are hoisted: references in earlier functions read the module variable.
function read() { return dep_1.x + def_1.default + (0, dep_1.y)(); }
const dep_1 = require("./dep");
const def_1 = __importDefault(require("./def"));
console.log(read());
