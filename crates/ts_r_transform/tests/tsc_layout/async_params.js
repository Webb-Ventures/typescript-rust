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
const a1 = (...args_1) => __awaiter(void 0, [...args_1], void 0, function* (a = 1, { b } = {}) { yield p; });
const a6 = (_a) => __awaiter(void 0, [_a], void 0, function* ({ b }) { return b; });
function f2(a_1) {
    var arguments_1 = arguments;
    return __awaiter(this, arguments, void 0, function* (a, b = 1) { return arguments_1[0]; });
}
function g1() { return __asyncGenerator(this, arguments, function* g1_1() { yield yield __await(void 0); return yield __await(void 0); }); }
class B {
    m() { return 1; }
}
class C extends B {
    g() { const _super = Object.create(null, {
        m: { get: () => super.m }
    }); return __asyncGenerator(this, arguments, function* g_1() { yield yield __await(_super.m.call(this)); }); }
}
function f9() {
    return __awaiter(this, void 0, void 0, function* () {
        if (p) {
            return;
        }
    });
}
function* gen() { const x = yield p; }
function f10() {
    return __awaiter(this, void 0, void 0, function* () {
        "use strict";
        yield p;
    });
}
function f11() {
    return __awaiter(this, void 0, void 0, function* () {
        // comment
        yield p; // trailing
    });
}
