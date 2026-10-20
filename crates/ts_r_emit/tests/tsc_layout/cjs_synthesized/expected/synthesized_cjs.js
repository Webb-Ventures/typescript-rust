"use strict";
var __decorate = (this && this.__decorate) || function (decorators, target, key, desc) {
    var c = arguments.length, r = c < 3 ? target : desc === null ? desc = Object.getOwnPropertyDescriptor(target, key) : desc, d;
    if (typeof Reflect === "object" && typeof Reflect.decorate === "function") r = Reflect.decorate(decorators, target, key, desc);
    else for (var i = decorators.length - 1; i >= 0; i--) if (d = decorators[i]) r = (c < 3 ? d(r) : c > 3 ? d(target, key, r) : d(target, key)) || r;
    return c > 3 && r && Object.defineProperty(target, key, r), r;
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.g = exports.E = exports.tagged = exports.C = exports.readFileSync = void 0;
/// <reference lib="es2022" />
var fs_1 = require("fs");
Object.defineProperty(exports, "readFileSync", { enumerable: true, get: function () { return fs_1.readFileSync; } });
let C = class C {
    m() { }
    p;
};
exports.C = C;
__decorate([
    dec
], C.prototype, "m", null);
__decorate([
    dec,
    dec
], C.prototype, "p", void 0);
exports.C = C = __decorate([
    dec
], C);
exports.tagged = String.raw `a${1}`;
const xy = 1, o = { xy };
var E;
(function (E) {
    E[E["A"] = 0] = "A";
})(E || (exports.E = E = {})); // trailing enum comment
exports.g = [1].map(async (x) => x);
exports.default = xy; // the default
