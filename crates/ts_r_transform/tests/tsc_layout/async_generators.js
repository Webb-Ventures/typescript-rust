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
var __await = (this && this.__await) || function (v) { return this instanceof __await ? (this.v = v, this) : new __await(v); }
var __asyncGenerator = (this && this.__asyncGenerator) || function (thisArg, _arguments, generator) {
    if (!Symbol.asyncIterator) throw new TypeError("Symbol.asyncIterator is not defined.");
    var g = generator.apply(thisArg, _arguments || []), i, q = [];
    return i = Object.create((typeof AsyncIterator === "function" ? AsyncIterator : Object).prototype), verb("next"), verb("throw"), verb("return", awaitReturn), i[Symbol.asyncIterator] = function () { return this; }, i;
    function awaitReturn(f) { return function (v) { return Promise.resolve(v).then(f, reject); }; }
    function verb(n, f) { if (g[n]) { i[n] = function (v) { return new Promise(function (a, b) { q.push([n, v, a, b]) > 1 || resume(n, v); }); }; if (f) i[n] = f(i[n]); } }
    function resume(n, v) { try { step(g[n](v)); } catch (e) { settle(q[0][3], e); } }
    function step(r) { r.value instanceof __await ? Promise.resolve(r.value.v).then(fulfill, reject) : settle(q[0][2], r); }
    function fulfill(value) { resume("next", value); }
    function reject(value) { resume("throw", value); }
    function settle(f, v) { if (f(v), q.shift(), q.length) resume(q[0][0], q[0][1]); }
};
var __asyncValues = (this && this.__asyncValues) || function (o) {
    if (!Symbol.asyncIterator) throw new TypeError("Symbol.asyncIterator is not defined.");
    var m = o[Symbol.asyncIterator], i;
    return m ? m.call(o) : (o = typeof __values === "function" ? __values(o) : o[Symbol.iterator](), i = {}, verb("next"), verb("throw"), verb("return"), i[Symbol.asyncIterator] = function () { return this; }, i);
    function verb(n) { i[n] = o[n] && function (v) { return new Promise(function (resolve, reject) { v = o[n](v), settle(resolve, reject, v.done, v.value); }); }; }
    function settle(resolve, reject, d, v) { Promise.resolve(v).then(function(v) { resolve({ value: v, done: d }); }, reject); }
};
var __asyncDelegator = (this && this.__asyncDelegator) || function (o) {
    var i, p;
    return i = {}, verb("next"), verb("throw", function (e) { throw e; }), verb("return"), i[Symbol.iterator] = function () { return this; }, i;
    function verb(n, f) { i[n] = o[n] ? function (v) { return (p = !p) ? { value: __await(o[n](v)), done: false } : f ? f(v) : v; } : f; }
};
function g1() { return __asyncGenerator(this, arguments, function* g1_1() { yield __await(p); yield yield __await(1); }); }
function g2(a) {
    return __asyncGenerator(this, arguments, function* g2_1() {
        const x = yield __await(yield* __asyncDelegator(__asyncValues(g1())));
        yield yield __await(yield __await(p));
        return yield __await(5);
    });
}
const g3 = function () { return __asyncGenerator(this, arguments, function* () { yield yield __await(1); }); };
class C {
    m() { return __asyncGenerator(this, arguments, function* m_1() { yield yield __await(1); }); }
}
function f1() {
    return __awaiter(this, void 0, void 0, function* () {
        var _a, e_1, _b, _c;
        try {
            for (var _d = true, it_1 = __asyncValues(it), it_1_1; it_1_1 = yield it_1.next(), _a = it_1_1.done, !_a; _d = true) {
                _c = it_1_1.value;
                _d = false;
                const x = _c;
                console.log(x);
            }
        }
        catch (e_1_1) { e_1 = { error: e_1_1 }; }
        finally {
            try {
                if (!_d && !_a && (_b = it_1.return)) yield _b.call(it_1);
            }
            finally { if (e_1) throw e_1.error; }
        }
    });
}
function f2() {
    return __awaiter(this, void 0, void 0, function* () {
        var _a, e_2, _b, _c, _d, e_3, _e, _f;
        try {
            for (var _g = true, it_2 = __asyncValues(it), it_2_1; it_2_1 = yield it_2.next(), _a = it_2_1.done, !_a; _g = true) {
                _c = it_2_1.value;
                _g = false;
                let [a, b] = _c;
            }
        }
        catch (e_2_1) { e_2 = { error: e_2_1 }; }
        finally {
            try {
                if (!_g && !_a && (_b = it_2.return)) yield _b.call(it_2);
            }
            finally { if (e_2) throw e_2.error; }
        }
        try {
            for (var _h = true, xs_1 = __asyncValues(xs), xs_1_1; xs_1_1 = yield xs_1.next(), _d = xs_1_1.done, !_d; _h = true) {
                _f = xs_1_1.value;
                _h = false;
                x1 = _f;
            }
        }
        catch (e_3_1) { e_3 = { error: e_3_1 }; }
        finally {
            try {
                if (!_h && !_d && (_e = xs_1.return)) yield _e.call(xs_1);
            }
            finally { if (e_3) throw e_3.error; }
        }
    });
}
function g4() {
    return __asyncGenerator(this, arguments, function* g4_1() {
        var _a, e_4, _b, _c;
        try {
            for (var _d = true, it_3 = __asyncValues(it), it_3_1; it_3_1 = yield __await(it_3.next()), _a = it_3_1.done, !_a; _d = true) {
                _c = it_3_1.value;
                _d = false;
                const x = _c;
                yield yield __await(x);
            }
        }
        catch (e_4_1) { e_4 = { error: e_4_1 }; }
        finally {
            try {
                if (!_d && !_a && (_b = it_3.return)) yield __await(_b.call(it_3));
            }
            finally { if (e_4) throw e_4.error; }
        }
    });
}
const o1 = Object.assign(Object.assign({ a: 1 }, xs), { b: 2 });
const o2 = Object.assign({}, xs);
const o3 = Object.assign(Object.assign({}, xs), o1);
