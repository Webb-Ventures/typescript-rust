"use strict";
var __awaiter = (this && this.__awaiter) || function (thisArg, _arguments, P, generator) {
    function adopt(value) { return value instanceof P ? value : new P(function (resolve) { resolve(value); }); }
    return new (P || (P = Promise))(function (resolve, reject) {
        function fulfilled(value) { try { step(generator.next(value)); } catch (e) { reject(e); } }
        function rejected(value) { try { step(generator["throw"](value)); } catch (e) { reject(e); } }
        function step(result) { result.done ? resolve(result.value) : adopt(result.value).then(fulfilled, rejected); }
        step((generator = generator.apply(thisArg, _arguments || [])).next());
    });
};
const a3 = () => __awaiter(void 0, void 0, void 0, function* () { return this; });
class B {
    constructor() {
        this.x = 1;
    }
    m() { return 1; }
}
class C extends B {
    o() {
        const _superIndex = name => super[name];
        const _super = Object.create(null, {
            m: { get: () => super.m }
        });
        return __awaiter(this, void 0, void 0, function* () { return _superIndex("y") + _super.m.call(this); });
    }
    q(k) {
        const _superIndex = name => super[name];
        return __awaiter(this, void 0, void 0, function* () { return _superIndex(k).call(this, 1); });
    }
    r() {
        const _super = Object.create(null, {
            m: { get: () => super.m, set: v => super.m = v },
            x: { get: () => super.x, set: v => super.x = v }
        });
        return __awaiter(this, void 0, void 0, function* () { const f = () => _super.m.call(this); _super.x++; });
    }
    s() {
        const _super = Object.create(null, {
            x: { get: () => super.x, set: v => super.x = v }
        });
        return __awaiter(this, void 0, void 0, function* () { [_super.x] = [1]; });
    }
}
const o = {
    __proto__: B.prototype,
    m() {
        const _super = Object.create(null, {
            m: { get: () => super.m }
        });
        return __awaiter(this, void 0, void 0, function* () { return _super.m.call(this); });
    },
};
function f6(a) {
    return __awaiter(this, void 0, void 0, function* () { yield p; });
}
function f7() {
    return __awaiter(this, void 0, void 0, function* () {
        var _a = 1;
        const g = () => __awaiter(this, void 0, void 0, function* () { yield p; });
    });
}
function f8(a, b) {
    var arguments_1 = arguments;
    return __awaiter(this, void 0, void 0, function* () { return arguments_1[0]; });
}
