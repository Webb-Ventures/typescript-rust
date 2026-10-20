"use strict";
var _a, _b, _c;
var _d, _e;
var _f;
var _g;
function f(o) {
    var _a;
    var _b;
    const r = (_b = (_a = o === null || o === void 0 ? void 0 : o.x) === null || _a === void 0 ? void 0 : _a.y) !== null && _b !== void 0 ? _b : o === null || o === void 0 ? void 0 : o.z;
    return () => { const q = o === null || o === void 0 ? void 0 : o.w; return q !== null && q !== void 0 ? q : r; };
}
let t = a;
t || (t = b);
(_f = t.x) !== null && _f !== void 0 ? _f : (t.x = 3);
t[_g = a.b] && (t[_g] = 4);
t.y || (t.y = b);
let e = Math.pow(2, 3);
e = Math.pow(e, 2);
(_a = a.b).c = Math.pow(_a.c, 2);
(_b = a)[_c = b()] = Math.pow(_b[_c], Math.pow(a, 2));
a === null || a === void 0 ? void 0 : a.b();
(_d = a.b) === null || _d === void 0 ? void 0 : _d.call(a);
(_e = a === null || a === void 0 ? void 0 : a[0]) === null || _e === void 0 ? void 0 : _e.call(a, 1);
a === null || a === void 0 ? true : delete a.b;
const n1 = 1000000, n2 = 255 !== null && 255 !== void 0 ? 255 : 0, n3 = 10n, n4 = 0.0001;
try {
    f(1);
}
catch (_h) { }
const s = () => t !== null && t !== void 0 ? t : (t = 1);
