var __classPrivateFieldGet = (this && this.__classPrivateFieldGet) || function (receiver, state, kind, f) {
    if (kind === "a" && !f) throw new TypeError("Private accessor was defined without a getter");
    if (typeof state === "function" ? receiver !== state || !f : !state.has(receiver)) throw new TypeError("Cannot read private member from an object whose class did not declare it");
    return kind === "m" ? f : kind === "a" ? f.call(receiver) : f ? f.value : state.get(receiver);
};
var _B_instances, _B_m;
// @target: es2015
// @module: esnext
class A {
    ;
}
class B {
    constructor() {
        _B_instances.add(this);
        this.x = 1;
        this.z = class {
            ;
        };
    }
    ; // after a lowered private method
    ;
    get y() { return __classPrivateFieldGet(this, _B_instances, "m", _B_m).call(this); }
    ;
}
_B_instances = new WeakSet(), _B_m = function _B_m() { return 1; };
export { A, B };
