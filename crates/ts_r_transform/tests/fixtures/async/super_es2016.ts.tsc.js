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
class Derived extends Base {
    test(key) {
        const _superIndex = (function (geti, seti) {
            const cache = Object.create(null);
            return name => cache[name] || (cache[name] = { get value() { return geti(name); }, set value(v) { seti(name, v); } });
        })(name => super[name], (name, value) => super[name] = value);
        const _super = Object.create(null, {
            foo: { get: () => super.foo, set: v => super.foo = v }
        });
        return __awaiter(this, void 0, void 0, function* () {
            var _a, _b, _c, _d;
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
                yield (_a = _super.foo) === null || _a === void 0 ? void 0 : _a.name,
                yield (_b = _superIndex(key).value) === null || _b === void 0 ? void 0 : _b.name,
                yield _super.foo.call(this, 1, 2),
                yield _superIndex(key).value.call(this, 1, 2),
                yield (_c = _super.foo) === null || _c === void 0 ? void 0 : _c.call(this, 1, 2),
                yield (_d = _superIndex(key).value) === null || _d === void 0 ? void 0 : _d.call(this, 1, 2),
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
let fn = () => __awaiter(void 0, void 0, void 0, function* () {
    return class extends Base {
        constructor() {
            super(...arguments);
            this.a = super.a;
            this.b = () => super.b;
        }
        c() { return super.c; }
        d() { return () => super.d; }
    };
});
class Derived2 extends Base {
    constructor() {
        const _super = Object.create(null, {
            foo: { get: () => super.foo }
        });
        super(...arguments);
        this.b = () => __awaiter(this, void 0, void 0, function* () { var _a, _b; return _b = class {
                constructor() {
                    this[_a] = 123;
                }
            },
            _a = _super.foo,
            _b; });
    }
    a() {
        const _super = Object.create(null, {
            foo: { get: () => super.foo }
        });
        return __awaiter(this, void 0, void 0, function* () { var _a, _b; return _b = class {
                constructor() {
                    this[_a] = 123;
                }
            },
            _a = _super.foo,
            _b; });
    }
}
for (let i = 0; i < 3; i++) {
    objs.push({
        __proto__: { foo() { return i; } },
        bar() {
            const _super = Object.create(null, {
                foo: { get: () => super.foo }
            });
            return __awaiter(this, void 0, void 0, function* () { return _super.foo.call(this); });
        },
    });
}
