"use strict";
var __runInitializers = (this && this.__runInitializers) || function (thisArg, initializers, value) {
    var useValue = arguments.length > 2;
    for (var i = 0; i < initializers.length; i++) {
        value = useValue ? initializers[i].call(thisArg, value) : initializers[i].call(thisArg);
    }
    return useValue ? value : void 0;
};
var __esDecorate = (this && this.__esDecorate) || function (ctor, descriptorIn, decorators, contextIn, initializers, extraInitializers) {
    function accept(f) { if (f !== void 0 && typeof f !== "function") throw new TypeError("Function expected"); return f; }
    var kind = contextIn.kind, key = kind === "getter" ? "get" : kind === "setter" ? "set" : "value";
    var target = !descriptorIn && ctor ? contextIn["static"] ? ctor : ctor.prototype : null;
    var descriptor = descriptorIn || (target ? Object.getOwnPropertyDescriptor(target, contextIn.name) : {});
    var _, done = false;
    for (var i = decorators.length - 1; i >= 0; i--) {
        var context = {};
        for (var p in contextIn) context[p] = p === "access" ? {} : contextIn[p];
        for (var p in contextIn.access) context.access[p] = contextIn.access[p];
        context.addInitializer = function (f) { if (done) throw new TypeError("Cannot add initializers after decoration has completed"); extraInitializers.push(accept(f || null)); };
        var result = (0, decorators[i])(kind === "accessor" ? { get: descriptor.get, set: descriptor.set } : descriptor[key], context);
        if (kind === "accessor") {
            if (result === void 0) continue;
            if (result === null || typeof result !== "object") throw new TypeError("Object expected");
            if (_ = accept(result.get)) descriptor.get = _;
            if (_ = accept(result.set)) descriptor.set = _;
            if (_ = accept(result.init)) initializers.unshift(_);
        }
        else if (_ = accept(result)) {
            if (kind === "field") initializers.unshift(_);
            else descriptor[key] = _;
        }
    }
    if (target) Object.defineProperty(target, contextIn.name, descriptor);
    done = true;
};
var __propKey = (this && this.__propKey) || function (x) {
    return typeof x === "symbol" ? x : "".concat(x);
};
const key = "k";
let Fields = (() => {
    var _a;
    let _instanceExtraInitializers = [];
    let _static_s_decorators;
    let _static_s_initializers = [];
    let _static_s_extraInitializers = [];
    let _a_decorators;
    let _a_initializers = [];
    let _a_extraInitializers = [];
    let _member_decorators;
    let _member_initializers = [];
    let _member_extraInitializers = [];
    let _m_decorators;
    return class Fields {
        static { _a_decorators = [dec], _member_decorators = [dec], _a = __propKey(key); }
        static {
            const _metadata = typeof Symbol === "function" && Symbol.metadata ? Object.create(null) : void 0;
            _static_s_decorators = [dec];
            _m_decorators = [dec];
            __esDecorate(this, null, _m_decorators, { kind: "method", name: "m", static: false, private: false, access: { has: obj => "m" in obj, get: obj => obj.m }, metadata: _metadata }, null, _instanceExtraInitializers);
            __esDecorate(null, null, _static_s_decorators, { kind: "field", name: "s", static: true, private: false, access: { has: obj => "s" in obj, get: obj => obj.s, set: (obj, value) => { obj.s = value; } }, metadata: _metadata }, _static_s_initializers, _static_s_extraInitializers);
            __esDecorate(null, null, _a_decorators, { kind: "field", name: "a", static: false, private: false, access: { has: obj => "a" in obj, get: obj => obj.a, set: (obj, value) => { obj.a = value; } }, metadata: _metadata }, _a_initializers, _a_extraInitializers);
            __esDecorate(null, null, _member_decorators, { kind: "field", name: _a, static: false, private: false, access: { has: obj => _a in obj, get: obj => obj[_a], set: (obj, value) => { obj[_a] = value; } }, metadata: _metadata }, _member_initializers, _member_extraInitializers);
            if (_metadata) Object.defineProperty(this, Symbol.metadata, { enumerable: true, configurable: true, writable: true, value: _metadata });
        }
        static { this.s = __runInitializers(this, _static_s_initializers, 4); }
        m() { }
        constructor(p) {
            this.p = (__runInitializers(this, _instanceExtraInitializers), p);
            this.a = __runInitializers(this, _a_initializers, 1);
            this[_a] = (__runInitializers(this, _a_extraInitializers), __runInitializers(this, _member_initializers, 2));
            this.b = (__runInitializers(this, _member_extraInitializers), 3);
        }
        static {
            __runInitializers(this, _static_s_extraInitializers);
        }
    };
})();
let Derived = (() => {
    let _classSuper = Fields;
    let _c_decorators;
    let _c_initializers = [];
    let _c_extraInitializers = [];
    return class Derived extends _classSuper {
        static {
            const _metadata = typeof Symbol === "function" && Symbol.metadata ? Object.create(_classSuper[Symbol.metadata] ?? null) : void 0;
            _c_decorators = [dec];
            __esDecorate(null, null, _c_decorators, { kind: "field", name: "c", static: false, private: false, access: { has: obj => "c" in obj, get: obj => obj.c, set: (obj, value) => { obj.c = value; } }, metadata: _metadata }, _c_initializers, _c_extraInitializers);
            if (_metadata) Object.defineProperty(this, Symbol.metadata, { enumerable: true, configurable: true, writable: true, value: _metadata });
        }
        constructor() {
            super(...arguments);
            this.c = __runInitializers(this, _c_initializers, 5);
            __runInitializers(this, _c_extraInitializers);
        }
    };
})();
