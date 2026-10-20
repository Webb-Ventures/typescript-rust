var __decorate = (this && this.__decorate) || function (decorators, target, key, desc) {
    var c = arguments.length, r = c < 3 ? target : desc === null ? desc = Object.getOwnPropertyDescriptor(target, key) : desc, d;
    if (typeof Reflect === "object" && typeof Reflect.decorate === "function") r = Reflect.decorate(decorators, target, key, desc);
    else for (var i = decorators.length - 1; i >= 0; i--) if (d = decorators[i]) r = (c < 3 ? d(r) : c > 3 ? d(target, key, r) : d(target, key)) || r;
    return c > 3 && r && Object.defineProperty(target, key, r), r;
};
var __metadata = (this && this.__metadata) || function (k, v) {
    if (typeof Reflect === "object" && typeof Reflect.metadata === "function") return Reflect.metadata(k, v);
};
var __param = (this && this.__param) || function (paramIndex, decorator) {
    return function (target, key) { decorator(target, key, paramIndex); }
};
var _a, _b;
// @target: es2022
// @experimentalDecorators: true
// @emitDecoratorMetadata: true
import { Imp } from "./imp";
var En;
(function (En) {
    En[En["A"] = 0] = "A";
})(En || (En = {}));
class Loc {
}
export class A {
    p1;
    p2;
    p3;
    p4;
    p5;
    p6;
    p7;
    p8;
    p9;
    p10;
    p11;
    p12;
    p13;
    p14;
    p15;
    static s1 = 1;
    m(a, b) { return ""; }
    get g() { return 1; }
    set g2(v) { }
    async am() { }
    ["comp"]() { }
}
__decorate([
    dec,
    __metadata("design:type", Number)
], A.prototype, "p1", void 0);
__decorate([
    dec,
    __metadata("design:type", Object)
], A.prototype, "p2", void 0);
__decorate([
    dec,
    __metadata("design:type", Loc)
], A.prototype, "p3", void 0);
__decorate([
    dec,
    __metadata("design:type", typeof (_a = typeof Imp !== "undefined" && Imp) === "function" ? _a : Object)
], A.prototype, "p4", void 0);
__decorate([
    dec,
    __metadata("design:type", Object)
], A.prototype, "p5", void 0);
__decorate([
    dec,
    __metadata("design:type", Object)
], A.prototype, "p6", void 0);
__decorate([
    dec,
    __metadata("design:type", Number)
], A.prototype, "p7", void 0);
__decorate([
    dec,
    __metadata("design:type", Function)
], A.prototype, "p8", void 0);
__decorate([
    dec,
    __metadata("design:type", Array)
], A.prototype, "p9", void 0);
__decorate([
    dec,
    __metadata("design:type", String)
], A.prototype, "p10", void 0);
__decorate([
    dec,
    __metadata("design:type", Boolean)
], A.prototype, "p11", void 0);
__decorate([
    dec,
    __metadata("design:type", Object)
], A.prototype, "p12", void 0);
__decorate([
    dec,
    __metadata("design:type", Promise)
], A.prototype, "p13", void 0);
__decorate([
    dec,
    __metadata("design:type", BigInt)
], A.prototype, "p14", void 0);
__decorate([
    dec,
    __metadata("design:type", Symbol)
], A.prototype, "p15", void 0);
__decorate([
    dec,
    __param(0, dec),
    __metadata("design:type", Function),
    __metadata("design:paramtypes", [Number, Loc]),
    __metadata("design:returntype", String)
], A.prototype, "m", null);
__decorate([
    dec,
    __metadata("design:type", Number),
    __metadata("design:paramtypes", [])
], A.prototype, "g", null);
__decorate([
    dec,
    __metadata("design:type", String),
    __metadata("design:paramtypes", [String])
], A.prototype, "g2", null);
__decorate([
    dec,
    __metadata("design:type", Function),
    __metadata("design:paramtypes", []),
    __metadata("design:returntype", Promise)
], A.prototype, "am", null);
__decorate([
    dec,
    __metadata("design:type", Function),
    __metadata("design:paramtypes", []),
    __metadata("design:returntype", void 0)
], A.prototype, "comp", null);
__decorate([
    dec,
    __metadata("design:type", Object)
], A, "s1", void 0);
let B = class B {
    constructor(x) { }
};
B = __decorate([
    dec,
    __metadata("design:paramtypes", [typeof (_b = typeof Imp !== "undefined" && Imp) === "function" ? _b : Object])
], B);
export default B;
