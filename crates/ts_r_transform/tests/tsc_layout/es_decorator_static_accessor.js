"use strict";
let C = (() => {
    var _C_z_accessor_storage, _C_z_get, _C_z_set, _C_z_1_accessor_storage;
    let _classDecorators = [dec];
    let _classDescriptor;
    let _classExtraInitializers = [];
    let _classThis;
    let _static_private_z_decorators;
    let _static_private_z_initializers = [];
    let _static_private_z_extraInitializers = [];
    let _static_private_z_descriptor;
    let _z_decorators;
    let _z_initializers = [];
    let _z_extraInitializers = [];
    var C = _classThis = class {
        get z() { return __classPrivateFieldGet(this, _C_z_1_accessor_storage, "f"); }
        set z(value) { __classPrivateFieldSet(this, _C_z_1_accessor_storage, value, "f"); }
        constructor() {
            _C_z_1_accessor_storage.set(this, __runInitializers(this, _z_initializers, 1));
            __runInitializers(this, _z_extraInitializers);
        }
    };
    _C_z_1_accessor_storage = new WeakMap();
    _C_z_get = function _C_z_get() { return _static_private_z_descriptor.get.call(this); };
    _C_z_set = function _C_z_set(value) { return _static_private_z_descriptor.set.call(this, value); };
    __setFunctionName(_classThis, "C");
    (() => {
        const _metadata = typeof Symbol === "function" && Symbol.metadata ? Object.create(null) : void 0;
        _z_decorators = [dec];
        _static_private_z_decorators = [dec];
        __esDecorate(_classThis, _static_private_z_descriptor = { get: __setFunctionName(function () { return __classPrivateFieldGet(_classThis, _classThis, "f", _C_z_accessor_storage); }, "#z", "get"), set: __setFunctionName(function (value) { __classPrivateFieldSet(_classThis, _classThis, value, "f", _C_z_accessor_storage); }, "#z", "set") }, _static_private_z_decorators, { kind: "accessor", name: "#z", static: true, private: true, access: { has: obj => __classPrivateFieldIn(_classThis, obj), get: obj => __classPrivateFieldGet(obj, _classThis, "a", _C_z_get), set: (obj, value) => { __classPrivateFieldSet(obj, _classThis, value, "a", _C_z_set); } }, metadata: _metadata }, _static_private_z_initializers, _static_private_z_extraInitializers);
        __esDecorate(_classThis, null, _z_decorators, { kind: "accessor", name: "z", static: false, private: false, access: { has: obj => "z" in obj, get: obj => obj.z, set: (obj, value) => { obj.z = value; } }, metadata: _metadata }, _z_initializers, _z_extraInitializers);
        __esDecorate(null, _classDescriptor = { value: _classThis }, _classDecorators, { kind: "class", name: _classThis.name, metadata: _metadata }, null, _classExtraInitializers);
        C = _classThis = _classDescriptor.value;
        if (_metadata) Object.defineProperty(_classThis, Symbol.metadata, { enumerable: true, configurable: true, writable: true, value: _metadata });
    })();
    _C_z_accessor_storage = { value: __runInitializers(_classThis, _static_private_z_initializers, 1) };
    (() => {
        __runInitializers(_classThis, _static_private_z_extraInitializers);
        __runInitializers(_classThis, _classExtraInitializers);
    })();
    return C = _classThis;
})();
