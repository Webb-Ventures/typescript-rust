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
var __asyncValues = (this && this.__asyncValues) || function (o) {
    if (!Symbol.asyncIterator) throw new TypeError("Symbol.asyncIterator is not defined.");
    var m = o[Symbol.asyncIterator], i;
    return m ? m.call(o) : (o = typeof __values === "function" ? __values(o) : o[Symbol.iterator](), i = {}, verb("next"), verb("throw"), verb("return"), i[Symbol.asyncIterator] = function () { return this; }, i);
    function verb(n) { i[n] = o[n] && function (v) { return new Promise(function (resolve, reject) { v = o[n](v), settle(resolve, reject, v.done, v.value); }); }; }
    function settle(resolve, reject, d, v) { Promise.resolve(v).then(function(v) { resolve({ value: v, done: d }); }, reject); }
};
function f1(c) {
    return __awaiter(this, void 0, void 0, function* () {
        var _a, c_1, c_1_1;
        var _b, e_1, _c, _d;
        try {
            for (_a = true, c_1 = __asyncValues(c); c_1_1 = yield c_1.next(), _b = c_1_1.done, !_b; _a = true) {
                _d = c_1_1.value;
                _a = false;
                const s = _d;
            }
        }
        catch (e_1_1) { e_1 = { error: e_1_1 }; }
        finally {
            try {
                if (!_a && !_b && (_c = c_1.return)) yield _c.call(c_1);
            }
            finally { if (e_1) throw e_1.error; }
        }
    });
}
function f2(c) {
    return __awaiter(this, void 0, void 0, function* () {
        var _a, e_2, _b, _c;
        try {
            for (var _d = true, it_1 = __asyncValues(it), it_1_1; it_1_1 = yield it_1.next(), _a = it_1_1.done, !_a; _d = true) {
                _c = it_1_1.value;
                _d = false;
                const s = _c;
            }
        }
        catch (e_2_1) { e_2 = { error: e_2_1 }; }
        finally {
            try {
                if (!_d && !_a && (_b = it_1.return)) yield _b.call(it_1);
            }
            finally { if (e_2) throw e_2.error; }
        }
    });
}
function f3() {
    return __awaiter(this, void 0, void 0, function* () {
        var _a, e_3, _b, _c;
        for (const s of it) { }
        try {
            for (var _d = true, it_2 = __asyncValues(it), it_2_1; it_2_1 = yield it_2.next(), _a = it_2_1.done, !_a; _d = true) {
                _c = it_2_1.value;
                _d = false;
                const s = _c;
            }
        }
        catch (e_3_1) { e_3 = { error: e_3_1 }; }
        finally {
            try {
                if (!_d && !_a && (_b = it_2.return)) yield _b.call(it_2);
            }
            finally { if (e_3) throw e_3.error; }
        }
    });
}
function f4(a) {
    return __awaiter(this, void 0, void 0, function* () {
        var a;
        a = 1;
        for (var i = 0;;) { }
    });
}
function f5(a) {
    return __awaiter(this, void 0, void 0, function* () {
        var x = 1;
        for (var i = 0;;) { }
    });
}
