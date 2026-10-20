var __decorate = (this && this.__decorate) || function (decorators, target, key, desc) {
    var c = arguments.length, r = c < 3 ? target : desc === null ? desc = Object.getOwnPropertyDescriptor(target, key) : desc, d;
    if (typeof Reflect === "object" && typeof Reflect.decorate === "function") r = Reflect.decorate(decorators, target, key, desc);
    else for (var i = decorators.length - 1; i >= 0; i--) if (d = decorators[i]) r = (c < 3 ? d(r) : c > 3 ? d(target, key, r) : d(target, key)) || r;
    return c > 3 && r && Object.defineProperty(target, key, r), r;
};
var __metadata = (this && this.__metadata) || function (k, v) {
    if (typeof Reflect === "object" && typeof Reflect.metadata === "function") return Reflect.metadata(k, v);
};
var C2_1;
var S;
(function (S) {
    S["A"] = "a";
})(S || (S = {}));
class X {
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
    m(...rest) { }
    n(x, y = 2) { }
}
__decorate([
    dec,
    __metadata("design:type", String)
], A.prototype, "p1", void 0);
__decorate([
    dec,
    __metadata("design:type", String)
], A.prototype, "p2", void 0);
__decorate([
    dec,
    __metadata("design:type", String)
], A.prototype, "p3", void 0);
__decorate([
    dec,
    __metadata("design:type", X)
], A.prototype, "p4", void 0);
__decorate([
    dec,
    __metadata("design:type", String)
], A.prototype, "p5", void 0);
__decorate([
    dec,
    __metadata("design:type", Date)
], A.prototype, "p6", void 0);
__decorate([
    dec,
    __metadata("design:type", Map)
], A.prototype, "p7", void 0);
__decorate([
    dec,
    __metadata("design:type", Array)
], A.prototype, "p8", void 0);
__decorate([
    dec,
    __metadata("design:type", Object)
], A.prototype, "p9", void 0);
__decorate([
    dec,
    __metadata("design:type", void 0)
], A.prototype, "p10", void 0);
__decorate([
    dec,
    __metadata("design:type", void 0)
], A.prototype, "p11", void 0);
__decorate([
    dec,
    __metadata("design:type", Function),
    __metadata("design:paramtypes", [String]),
    __metadata("design:returntype", void 0)
], A.prototype, "m", null);
__decorate([
    dec,
    __metadata("design:type", Function),
    __metadata("design:paramtypes", [Number, Object]),
    __metadata("design:returntype", void 0)
], A.prototype, "n", null);
export class NoCtor {
}
let B = class B {
};
B = __decorate([
    dec
], B);
export { B };
let C2 = C2_1 = class C2 {
    static make() { return new C2_1(); }
};
C2 = C2_1 = __decorate([
    dec
], C2);
