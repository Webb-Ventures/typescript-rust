"use strict";
function f1(x, b, c, _a) { var _b; if (x === void 0) { x = 1; } if (b === void 0) { b = (_b = a.x) === null || _b === void 0 ? void 0 : _b.b; } if (c === void 0) { c = 2; } var { d } = _a === void 0 ? a : _a; return b; }
function f2(x, _a, z) { var _b; var { [(_b = a.x) === null || _b === void 0 ? void 0 : _b.b]: y } = _a; if (z === void 0) { z = 3; } }
const f3 = (b) => { var _a; if (b === void 0) { b = (_a = a.x) !== null && _a !== void 0 ? _a : 1; } return b; };
function f4(b) {
    "use strict";
    var _a;
    if (b === void 0) { b = (_a = a.x) === null || _a === void 0 ? void 0 : _a.b; }
    return b;
}
class C {
    m(b) { var _a; if (b === void 0) { b = (_a = a.x) === null || _a === void 0 ? void 0 : _a.c; } }
    constructor(p) {
        var _a;
        if (p === void 0) { p = (_a = a.x) === null || _a === void 0 ? void 0 : _a.d; }
        this.p = p;
    }
}
