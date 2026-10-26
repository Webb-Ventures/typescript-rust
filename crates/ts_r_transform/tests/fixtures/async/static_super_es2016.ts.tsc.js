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
var _a, _b;
class Derived extends Base {
    static test(key) {
        const _superIndex = (function (geti, seti) {
            const cache = Object.create(null);
            return name => cache[name] || (cache[name] = { get value() { return geti(name); }, set value(v) { seti(name, v); } });
        })(name => super[name], (name, value) => super[name] = value);
        const _super = Object.create(null, {
            foo: { get: () => super.foo, set: v => super.foo = v }
        });
        return __awaiter(this, void 0, void 0, function* () {
            var _c, _d, _e, _f;
            return [
                yield _super.foo,
                yield _superIndex(key).value,
                yield ([_super.foo] = [0]),
                yield ([_superIndex(key).value] = [0]),
                yield (_super.foo = 1),
                yield (_superIndex(key).value = 1),
                yield (_super.foo += 2),
                yield (_superIndex(key).value += 2),
                yield ++_super.foo,
                yield ++_superIndex(key).value,
                yield _super.foo++,
                yield _superIndex(key).value++,
                yield _super.foo.name,
                yield _superIndex(key).value.name,
                yield (_c = _super.foo) === null || _c === void 0 ? void 0 : _c.name,
                yield (_d = _superIndex(key).value) === null || _d === void 0 ? void 0 : _d.name,
                yield _super.foo.call(this, 1, 2),
                yield _superIndex(key).value.call(this, 1, 2),
                yield (_e = _super.foo) === null || _e === void 0 ? void 0 : _e.call(this, 1, 2),
                yield (_f = _superIndex(key).value) === null || _f === void 0 ? void 0 : _f.call(this, 1, 2),
                yield (() => _super.foo)(),
                yield (() => _superIndex(key).value)(),
                yield (() => _super.foo.call(this))(),
                yield (() => _superIndex(key).value.call(this))(),
                yield _super.foo ``,
                yield _superIndex(key).value ``,
            ];
        });
    }
}
let fn = () => __awaiter(void 0, void 0, void 0, function* () { var _c, _d; return _c = class extends (_d = Base) {
        static c() { return super.c; }
        static d() { return () => super.d; }
    },
    _c.a = Reflect.get(_d, "a", _c),
    _c.b = () => Reflect.get(_d, "b", _c),
    _c; });
class Derived2 extends (_b = Base) {
}
_a = Derived2;
Derived2.b = () => __awaiter(void 0, void 0, void 0, function* () { return Reflect.get(_b, "foo", _a); });
