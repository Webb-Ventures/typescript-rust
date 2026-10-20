"use strict";
var __setFunctionName = (this && this.__setFunctionName) || function (f, name, prefix) {
    if (typeof name === "symbol") name = name.description ? "[".concat(name.description, "]") : "";
    return Object.defineProperty(f, "name", { configurable: true, value: prefix ? "".concat(prefix, " ", name) : name });
};
var _a, _b, _c;
class A {
    constructor(p, r) {
        Object.defineProperty(this, "p", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: p
        });
        Object.defineProperty(this, "r", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: r
        });
        Object.defineProperty(this, "x", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: void 0
        });
        Object.defineProperty(this, "y", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 1
        });
        Object.defineProperty(this, "q", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 2
        });
        Object.defineProperty(this, _b, {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 3
        });
        Object.defineProperty(this, 0, {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 4
        });
        Object.defineProperty(this, "str", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 5
        });
        console.log(p);
    }
}
_a = A, _b = k;
Object.defineProperty(A, "t", {
    enumerable: true,
    configurable: true,
    writable: true,
    value: _a
});
class B extends A {
    constructor() {
        super(...arguments);
        Object.defineProperty(this, "z", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 1
        });
    }
}
class C extends A {
    constructor() {
        console.log(1);
        super(1, "");
        Object.defineProperty(this, "w", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 2
        });
        console.log(2);
    }
}
const D = (_c = class {
        constructor() {
            Object.defineProperty(this, "a", {
                enumerable: true,
                configurable: true,
                writable: true,
                value: 1
            });
        }
    },
    __setFunctionName(_c, "D"),
    Object.defineProperty(_c, "b", {
        enumerable: true,
        configurable: true,
        writable: true,
        value: 2
    }),
    _c);
