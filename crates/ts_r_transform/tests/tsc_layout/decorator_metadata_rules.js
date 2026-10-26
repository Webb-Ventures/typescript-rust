"use strict";
var _a, _b;
Object.defineProperty(exports, "__esModule", { value: true });
exports.C = void 0;
// @target: es2015
// @module: commonjs
// @strict: true
// @experimentalDecorators: true
// @emitDecoratorMetadata: true
// @noEmitHelpers: true
const missing_a_1 = require("./missing-a");
const missing_r_1 = require("./missing-r");
class C {
    get x() { return null; }
    set x(v) { }
    m(...rest) { }
}
exports.C = C;
__decorate([
    dec,
    __metadata("design:type", typeof BigInt === "function" ? BigInt : Object)
], C.prototype, "big", void 0);
__decorate([
    dec,
    __metadata("design:type", Object),
    __metadata("design:paramtypes", [Object])
], C.prototype, "x", null);
__decorate([
    dec,
    __metadata("design:type", Function),
    __metadata("design:paramtypes", [typeof (_a = typeof missing_r_1.R !== "undefined" && missing_r_1.R) === "function" ? _a : Object]),
    __metadata("design:returntype", void 0)
], C.prototype, "m", null);
__decorate([
    dec,
    __metadata("design:type", Object)
], C.prototype, "u", void 0);
