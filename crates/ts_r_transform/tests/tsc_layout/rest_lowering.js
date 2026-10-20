"use strict";
var __rest = (this && this.__rest) || function (s, e) {
    var t = {};
    for (var p in s) if (Object.prototype.hasOwnProperty.call(s, p) && e.indexOf(p) < 0)
        t[p] = s[p];
    if (s != null && typeof Object.getOwnPropertySymbols === "function")
        for (var i = 0, p = Object.getOwnPropertySymbols(s); i < p.length; i++) {
            if (e.indexOf(p[i]) < 0 && Object.prototype.propertyIsEnumerable.call(s, p[i]))
                t[p[i]] = s[p[i]];
        }
    return t;
};
var __asyncValues = (this && this.__asyncValues) || function (o) {
    if (!Symbol.asyncIterator) throw new TypeError("Symbol.asyncIterator is not defined.");
    var m = o[Symbol.asyncIterator], i;
    return m ? m.call(o) : (o = typeof __values === "function" ? __values(o) : o[Symbol.iterator](), i = {}, verb("next"), verb("throw"), verb("return"), i[Symbol.asyncIterator] = function () { return this; }, i);
    function verb(n) { i[n] = o[n] && function (v) { return new Promise(function (resolve, reject) { v = o[n](v), settle(resolve, reject, v.done, v.value); }); }; }
    function settle(resolve, reject, d, v) { Promise.resolve(v).then(function(v) { resolve({ value: v, done: d }); }, reject); }
};
var _a;
async function g() {
    var _a, e_1, _b, _c, _d, e_2, _e, _f, _g, e_3, _h, _j, _k, e_4, _l, _m, _o, e_5, _p, _q;
    try {
        for (var _r = true, _s = __asyncValues(f()), _t; _t = await _s.next(), _a = _t.done, !_a; _r = true) {
            _c = _t.value;
            _r = false;
            const x = _c;
        }
    }
    catch (e_1_1) { e_1 = { error: e_1_1 }; }
    finally {
        try {
            if (!_r && !_a && (_b = _s.return)) await _b.call(_s);
        }
        finally { if (e_1) throw e_1.error; }
    }
    try {
        for (var _u = true, _v = __asyncValues(o1.items), _w; _w = await _v.next(), _d = _w.done, !_d; _u = true) {
            _f = _w.value;
            _u = false;
            const x = _f;
        }
    }
    catch (e_2_1) { e_2 = { error: e_2_1 }; }
    finally {
        try {
            if (!_u && !_d && (_e = _v.return)) await _e.call(_v);
        }
        finally { if (e_2) throw e_2.error; }
    }
    try {
        lbl: for (var _x = true, it_1 = __asyncValues(it), it_1_1; it_1_1 = await it_1.next(), _g = it_1_1.done, !_g; _x = true) {
            _j = it_1_1.value;
            _x = false;
            const x = _j;
            continue lbl;
        }
    }
    catch (e_3_1) { e_3 = { error: e_3_1 }; }
    finally {
        try {
            if (!_x && !_g && (_h = it_1.return)) await _h.call(it_1);
        }
        finally { if (e_3) throw e_3.error; }
    }
    try {
        for (var _y = true, it_2 = __asyncValues(it), it_2_1; it_2_1 = await it_2.next(), _k = it_2_1.done, !_k; _y = true) {
            _m = it_2_1.value;
            _y = false;
            const x = _m;
            f();
        }
    }
    catch (e_4_1) { e_4 = { error: e_4_1 }; }
    finally {
        try {
            if (!_y && !_k && (_l = it_2.return)) await _l.call(it_2);
        }
        finally { if (e_4) throw e_4.error; }
    }
    try {
        for (var _z = true, it_3 = __asyncValues(it), it_3_1; it_3_1 = await it_3.next(), _o = it_3_1.done, !_o; _z = true) {
            _q = it_3_1.value;
            _z = false;
            let _0 = _q;
            const { a } = _0, r = __rest(_0, ["a"]);
        }
    }
    catch (e_5_1) { e_5 = { error: e_5_1 }; }
    finally {
        try {
            if (!_z && !_o && (_p = it_3.return)) await _p.call(it_3);
        }
        finally { if (e_5) throw e_5.error; }
    }
}
const s1 = Object.assign(Object.assign({ a: 1 }, o1), { b: 2 });
const s2 = Object.assign(Object.assign({}, o1), { get x() { return 1; }, m() { } });
const _b = f(), { a } = _b, r1 = __rest(_b, ["a"]);
let _c = o1.p, { b } = _c, r2 = __rest(_c, ["b"]);
const _d = o1, _e = k, c = _d[_e], r3 = __rest(_d, [typeof _e === "symbol" ? _e : _e + ""]);
const _f = o1.d, { e } = _f, r4 = __rest(_f, ["e"]), r5 = __rest(o1, ["d"]);
const [_g] = o1, r6 = __rest(_g, []);
const fa = (_a) => {
    var { a } = _a, r = __rest(_a, ["a"]);
    return r;
};
function fb(x, _a = {}) { var { a } = _a, r = __rest(_a, ["a"]); return r; }
let q1, q2;
(_a = f(), { q1 } = _a, q2 = __rest(_a, ["q1"]));
const v = ({ q1 } = o1, q2 = __rest(o1, ["q1"]), o1);
for (let _h of [o1]) {
    ({ q1 } = _h, q2 = __rest(_h, ["q1"]));
}
try { }
catch (_j) {
    var { a } = _j, r = __rest(_j, ["a"]);
}
const { "s-t": st, 1: one } = o1, r7 = __rest(o1, ["s-t", "1"]);
