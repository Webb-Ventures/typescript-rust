var __setFunctionName = (this && this.__setFunctionName) || function (f, name, prefix) {
    if (typeof name === "symbol") name = name.description ? "[".concat(name.description, "]") : "";
    return Object.defineProperty(f, "name", { configurable: true, value: prefix ? "".concat(prefix, " ", name) : name });
};
var _a;
var _b, _c, _d, _e, _f, _g;
class A {
    constructor() {
        this.x = 2;
    }
    static m() { return _b.s; }
}
_b = A, _c = k;
A.s = 1;
(() => {
    console.log(_b.s);
})();
A[_c] = 3;
const B = (_d = class {
    },
    __setFunctionName(_d, "B"),
    _d.s = 1,
    (() => {
        _d.s++;
    })(),
    _d);
class C extends (_f = A) {
}
_e = C;
C.u = Reflect.get(_f, "m", _e).call(_e);
(() => {
    Reflect.get(_f, "m", _e).call(_e);
})();
class D {
}
_g = D;
D.self = _g;
D.f = () => _g;
export class E {
}
E.s = 1;
class default_1 {
}
default_1.s = 1;
export default default_1;
function f() { var _a; return _a = class {
    },
    _a.s = _a,
    _a; }
(_a = a.x) === null || _a === void 0 ? void 0 : _a.b;
function g() { var _a; return (_a = a.y) === null || _a === void 0 ? void 0 : _a.b; }
function h() { var _a; return _a = class {
    },
    _a.s = _a,
    _a; }
function k() { var _a; var _h; var x = (_a = a.y) === null || _a === void 0 ? void 0 : _a.b; return _h = class {
    },
    _h.s = _h,
    _h; }
