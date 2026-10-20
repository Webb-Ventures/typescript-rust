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
var __setFunctionName = (this && this.__setFunctionName) || function (f, name, prefix) {
    if (typeof name === "symbol") name = name.description ? "[".concat(name.description, "]") : "";
    return Object.defineProperty(f, "name", { configurable: true, value: prefix ? "".concat(prefix, " ", name) : name });
};
const key = "computed";
let Service = (() => {
    var _a, _b;
    let _classDecorators = [dec];
    let _classDescriptor;
    let _classExtraInitializers = [];
    let _classThis;
    let _staticExtraInitializers = [];
    let _instanceExtraInitializers = [];
    let _static_create_decorators;
    let _name_decorators;
    let _name_initializers = [];
    let _name_extraInitializers = [];
    let _count_decorators;
    let _count_initializers = [];
    let _count_extraInitializers = [];
    let _member_decorators;
    let _member_initializers = [];
    let _member_extraInitializers = [];
    let _enabled_decorators;
    let _enabled_initializers = [];
    let _enabled_extraInitializers = [];
    let _private_secret_decorators;
    let _private_secret_initializers = [];
    let _private_secret_extraInitializers = [];
    let _get_label_decorators;
    let _set_label_decorators;
    let _private_helper_decorators;
    let _private_helper_descriptor;
    let _private_flag_decorators;
    let _private_flag_initializers = [];
    let _private_flag_extraInitializers = [];
    let _private_flag_descriptor;
    var Service = class {
        static { _classThis = this; }
        static {
            const _metadata = typeof Symbol === "function" && Symbol.metadata ? Object.create(null) : void 0;
            _enabled_decorators = [dec];
            _private_secret_decorators = [dec];
            _static_create_decorators = [dec];
            _get_label_decorators = [dec];
            _set_label_decorators = [dec];
            _private_helper_decorators = [dec];
            _private_flag_decorators = [dec];
            __esDecorate(this, null, _static_create_decorators, { kind: "method", name: "create", static: true, private: false, access: { has: obj => "create" in obj, get: obj => obj.create }, metadata: _metadata }, null, _staticExtraInitializers);
            __esDecorate(this, null, _enabled_decorators, { kind: "accessor", name: "enabled", static: false, private: false, access: { has: obj => "enabled" in obj, get: obj => obj.enabled, set: (obj, value) => { obj.enabled = value; } }, metadata: _metadata }, _enabled_initializers, _enabled_extraInitializers);
            __esDecorate(this, null, _get_label_decorators, { kind: "getter", name: "label", static: false, private: false, access: { has: obj => "label" in obj, get: obj => obj.label }, metadata: _metadata }, null, _instanceExtraInitializers);
            __esDecorate(this, null, _set_label_decorators, { kind: "setter", name: "label", static: false, private: false, access: { has: obj => "label" in obj, set: (obj, value) => { obj.label = value; } }, metadata: _metadata }, null, _instanceExtraInitializers);
            __esDecorate(this, _private_helper_descriptor = { value: __setFunctionName(function () { return this.#secret; }, "#helper") }, _private_helper_decorators, { kind: "method", name: "#helper", static: false, private: true, access: { has: obj => #helper in obj, get: obj => obj.#helper }, metadata: _metadata }, null, _instanceExtraInitializers);
            __esDecorate(this, _private_flag_descriptor = { get: __setFunctionName(function () { return this.#flag_accessor_storage; }, "#flag", "get"), set: __setFunctionName(function (value) { this.#flag_accessor_storage = value; }, "#flag", "set") }, _private_flag_decorators, { kind: "accessor", name: "#flag", static: false, private: true, access: { has: obj => #flag in obj, get: obj => obj.#flag, set: (obj, value) => { obj.#flag = value; } }, metadata: _metadata }, _private_flag_initializers, _private_flag_extraInitializers);
            __esDecorate(null, null, _name_decorators, { kind: "field", name: "name", static: false, private: false, access: { has: obj => "name" in obj, get: obj => obj.name, set: (obj, value) => { obj.name = value; } }, metadata: _metadata }, _name_initializers, _name_extraInitializers);
            __esDecorate(null, null, _count_decorators, { kind: "field", name: "count", static: false, private: false, access: { has: obj => "count" in obj, get: obj => obj.count, set: (obj, value) => { obj.count = value; } }, metadata: _metadata }, _count_initializers, _count_extraInitializers);
            __esDecorate(null, null, _member_decorators, { kind: "field", name: _b, static: false, private: false, access: { has: obj => _b in obj, get: obj => obj[_b], set: (obj, value) => { obj[_b] = value; } }, metadata: _metadata }, _member_initializers, _member_extraInitializers);
            __esDecorate(null, null, _private_secret_decorators, { kind: "field", name: "#secret", static: false, private: true, access: { has: obj => #secret in obj, get: obj => obj.#secret, set: (obj, value) => { obj.#secret = value; } }, metadata: _metadata }, _private_secret_initializers, _private_secret_extraInitializers);
            __esDecorate(null, _classDescriptor = { value: _classThis }, _classDecorators, { kind: "class", name: _classThis.name, metadata: _metadata }, null, _classExtraInitializers);
            Service = _classThis = _classDescriptor.value;
            if (_metadata) Object.defineProperty(_classThis, Symbol.metadata, { enumerable: true, configurable: true, writable: true, value: _metadata });
        }
        id = __runInitializers(this, _instanceExtraInitializers);
        static instances = (__runInitializers(_classThis, _staticExtraInitializers), 0);
        name = __runInitializers(this, _name_initializers, "svc");
        count = (__runInitializers(this, _name_extraInitializers), __runInitializers(this, _count_initializers, 1));
        [(_name_decorators = [dec], _count_decorators = [(_a = registry).field.bind(_a)], _member_decorators = [dec(1)], _b = __propKey(key))] = (__runInitializers(this, _count_extraInitializers), __runInitializers(this, _member_initializers, 2));
        #enabled_accessor_storage = (__runInitializers(this, _member_extraInitializers), __runInitializers(this, _enabled_initializers, true));
        get enabled() { return this.#enabled_accessor_storage; }
        set enabled(value) { this.#enabled_accessor_storage = value; }
        #secret = (__runInitializers(this, _enabled_extraInitializers), __runInitializers(this, _private_secret_initializers, 3));
        static create() { return new Service(); }
        get label() { return this.name; }
        set label(v) { this.name = v; }
        get #helper() { return _private_helper_descriptor.value; }
        #flag_accessor_storage = (__runInitializers(this, _private_secret_extraInitializers), __runInitializers(this, _private_flag_initializers, false));
        get #flag() { return _private_flag_descriptor.get.call(this); }
        set #flag(value) { return _private_flag_descriptor.set.call(this, value); }
        constructor(id) {
            __runInitializers(this, _private_flag_extraInitializers);
            this.id = id;
            Service.instances++;
        }
        run() { return this.#helper() + (this.#flag ? 1 : 0); }
        static {
            __runInitializers(_classThis, _classExtraInitializers);
        }
    };
    return Service = _classThis;
})();
export { Service };
let Child = (() => {
    let _classSuper = Service;
    let _instanceExtraInitializers = [];
    let _method_decorators;
    return class Child extends _classSuper {
        static {
            const _metadata = typeof Symbol === "function" && Symbol.metadata ? Object.create(_classSuper[Symbol.metadata] ?? null) : void 0;
            _method_decorators = [registry.tag("child")];
            __esDecorate(this, null, _method_decorators, { kind: "method", name: "method", static: false, private: false, access: { has: obj => "method" in obj, get: obj => obj.method }, metadata: _metadata }, null, _instanceExtraInitializers);
            if (_metadata) Object.defineProperty(this, Symbol.metadata, { enumerable: true, configurable: true, writable: true, value: _metadata });
        }
        method() { }
        constructor() {
            super(...arguments);
            __runInitializers(this, _instanceExtraInitializers);
        }
    };
})();
export { Child };
export default (() => {
    let _classDecorators = [dec];
    let _classDescriptor;
    let _classExtraInitializers = [];
    let _classThis;
    var default_1 = class {
        static { _classThis = this; }
        static { __setFunctionName(_classThis, "default"); }
        static {
            const _metadata = typeof Symbol === "function" && Symbol.metadata ? Object.create(null) : void 0;
            __esDecorate(null, _classDescriptor = { value: _classThis }, _classDecorators, { kind: "class", name: _classThis.name, metadata: _metadata }, null, _classExtraInitializers);
            default_1 = _classThis = _classDescriptor.value;
            if (_metadata) Object.defineProperty(_classThis, Symbol.metadata, { enumerable: true, configurable: true, writable: true, value: _metadata });
            __runInitializers(_classThis, _classExtraInitializers);
        }
    };
    return default_1 = _classThis;
})();
const Named = (() => {
    let _classDecorators = [dec];
    let _classDescriptor;
    let _classExtraInitializers = [];
    let _classThis;
    let _static_x_decorators;
    let _static_x_initializers = [];
    let _static_x_extraInitializers = [];
    var class_1 = class {
        static { _classThis = this; }
        static { __setFunctionName(_classThis, "Named"); }
        static {
            const _metadata = typeof Symbol === "function" && Symbol.metadata ? Object.create(null) : void 0;
            _static_x_decorators = [dec];
            __esDecorate(null, null, _static_x_decorators, { kind: "field", name: "x", static: true, private: false, access: { has: obj => "x" in obj, get: obj => obj.x, set: (obj, value) => { obj.x = value; } }, metadata: _metadata }, _static_x_initializers, _static_x_extraInitializers);
            __esDecorate(null, _classDescriptor = { value: _classThis }, _classDecorators, { kind: "class", name: _classThis.name, metadata: _metadata }, null, _classExtraInitializers);
            class_1 = _classThis = _classDescriptor.value;
            if (_metadata) Object.defineProperty(_classThis, Symbol.metadata, { enumerable: true, configurable: true, writable: true, value: _metadata });
        }
        static x = __runInitializers(_classThis, _static_x_initializers, 1);
        static {
            __runInitializers(_classThis, _static_x_extraInitializers);
            __runInitializers(_classThis, _classExtraInitializers);
        }
    };
    return class_1 = _classThis;
})();
let assigned;
assigned = (() => {
    let _y_decorators;
    let _y_initializers = [];
    let _y_extraInitializers = [];
    return class {
        static { __setFunctionName(this, "assigned"); }
        static {
            const _metadata = typeof Symbol === "function" && Symbol.metadata ? Object.create(null) : void 0;
            _y_decorators = [dec];
            __esDecorate(null, null, _y_decorators, { kind: "field", name: "y", static: false, private: false, access: { has: obj => "y" in obj, get: obj => obj.y, set: (obj, value) => { obj.y = value; } }, metadata: _metadata }, _y_initializers, _y_extraInitializers);
            if (_metadata) Object.defineProperty(this, Symbol.metadata, { enumerable: true, configurable: true, writable: true, value: _metadata });
        }
        y = __runInitializers(this, _y_initializers, void 0);
        constructor() {
            __runInitializers(this, _y_extraInitializers);
        }
    };
})();
