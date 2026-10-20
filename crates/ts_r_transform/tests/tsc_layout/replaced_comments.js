"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.x = void 0;
// @target: es2022
// @module: commonjs
// @useDefineForClassFields: false
// header comment
const fs_1 = __importDefault(require("fs"));
// before b
const b_1 = require("./b");
/** doc */
exports.x = fs_1.default.readFileSync(b_1.b);
// before enum
var E;
(function (E) {
    E[E["A"] = 0] = "A";
})(E || (E = {}));
class K {
    constructor() {
        this.y = 1;
        // in ctor
        f();
    }
}
