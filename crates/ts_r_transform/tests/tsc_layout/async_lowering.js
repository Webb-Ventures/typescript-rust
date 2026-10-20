var __awaiter = (this && this.__awaiter) || function (thisArg, _arguments, P, generator) {
    function adopt(value) { return value instanceof P ? value : new P(function (resolve) { resolve(value); }); }
    return new (P || (P = Promise))(function (resolve, reject) {
        function fulfilled(value) { try { step(generator.next(value)); } catch (e) { reject(e); } }
        function rejected(value) { try { step(generator["throw"](value)); } catch (e) { reject(e); } }
        function step(result) { result.done ? resolve(result.value) : adopt(result.value).then(fulfilled, rejected); }
        step((generator = generator.apply(thisArg, _arguments || [])).next());
    });
};
function f1() {
    return __awaiter(this, void 0, void 0, function* () { yield p; });
}
function f2(a_1) {
    return __awaiter(this, arguments, void 0, function* (a, b = 1, ...c) {
        const x = yield p;
        return x + a;
    });
}
function f3() {
    var arguments_1 = arguments;
    return __awaiter(this, void 0, void 0, function* () { return arguments_1.length; });
}
const a1 = () => __awaiter(void 0, void 0, void 0, function* () { yield p; });
const a2 = (x) => __awaiter(void 0, void 0, void 0, function* () { return (yield p) + x; });
const a3 = () => __awaiter(void 0, void 0, void 0, function* () { return this; });
const a4 = function () {
    return __awaiter(this, void 0, void 0, function* () { return this; });
};
const a5 = function named() {
    var arguments_2 = arguments;
    return __awaiter(this, void 0, void 0, function* () { return arguments_2; });
};
class C {
    m() {
        return __awaiter(this, void 0, void 0, function* () { yield p; return this; });
    }
    static s() {
        const _super = Object.create(null, {
            toString: { get: () => super.toString }
        });
        return __awaiter(this, void 0, void 0, function* () { return _super.toString.call(this); });
    }
    n() {
        const _super = Object.create(null, {
            toString: { get: () => super.toString }
        });
        return __awaiter(this, void 0, void 0, function* () { return _super.toString.call(this); });
    }
    o() {
        const _superIndex = (function (geti, seti) {
            const cache = Object.create(null);
            return name => cache[name] || (cache[name] = { get value() { return geti(name); }, set value(v) { seti(name, v); } });
        })(name => super[name], (name, value) => super[name] = value);
        const _super = Object.create(null, {
            x: { get: () => super.x, set: v => super.x = v }
        });
        return __awaiter(this, void 0, void 0, function* () { _super.x = 1; return _superIndex("y").value; });
    }
}
const o = {
    m() {
        return __awaiter(this, void 0, void 0, function* () { yield p; });
    },
    ["k"]() {
        return __awaiter(this, void 0, void 0, function* () { });
    },
};
function outer() {
    return () => {
        var arguments_3 = arguments;
        return __awaiter(this, void 0, void 0, function* () { return arguments_3[0]; });
    };
}
function outer2() {
    const f = () => __awaiter(this, void 0, void 0, function* () { return this; });
}
function f4(_a, _b) {
    return __awaiter(this, arguments, void 0, function* ({ a }, [b]) { yield a; });
}
function f5() {
    return __awaiter(this, arguments, void 0, function* (x = await1()) { });
}
export default function () {
    return __awaiter(this, void 0, void 0, function* () { });
}
