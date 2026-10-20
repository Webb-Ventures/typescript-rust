"use strict";
var _A_x, _A_f, _a, _y, _z, _B_instances, _B_b, _B_m, _C_instances, _C_q, _b, _D_w, _P_instances, _c, _P_m, _P_x, _P_s, _P_g_get, _P_sg_get, _P_sg_set, _d, _Q_z, _A4_foo, _e, _C4_foo, _D4_instances, _D4_m, _f, _E4_s;
// @target: es2015
// @module: esnext
// @noEmitHelpers: true
class A {
    constructor() {
        _A_x.set(this, 1);
        _A_f.set(this, () => 1);
    }
    m(o) {
        var _g, _h, _j, _k, _l, _m, _o, _p;
        __classPrivateFieldSet(_g = this.a, _A_x, __classPrivateFieldGet(_g, _A_x, "f") + 1, "f");
        __classPrivateFieldSet(_h = o, _A_x, __classPrivateFieldGet(_h, _A_x, "f") * 2, "f");
        __classPrivateFieldSet(this.a, _A_x, 3, "f");
        __classPrivateFieldSet(_j = this.a, _A_x, (_k = __classPrivateFieldGet(_j, _A_x, "f"), _k++, _k), "f");
        const r = (__classPrivateFieldSet(_l = o, _A_x, (_o = __classPrivateFieldGet(_l, _A_x, "f"), _m = _o--, _o), "f"), _m);
        __classPrivateFieldGet(this, _A_f, "f").call(this);
        __classPrivateFieldGet((_p = this.a), _A_f, "f").call(_p, 1, 2);
        __classPrivateFieldGet(o, _A_f, "f").bind(o) ``;
        return __classPrivateFieldGet(this.a, _A_x, "f");
    }
}
_A_x = new WeakMap(), _A_f = new WeakMap();
foo((_a = class {
        constructor() {
            _y.set(this, 1);
        }
        get() { return __classPrivateFieldGet(this, _y, "f"); }
    },
    _y = new WeakMap(),
    _z = { value: 2 },
    _a));
class B extends A {
    constructor() {
        super(...arguments);
        _B_instances.add(this);
        _B_b.set(this, 1);
    }
}
_B_b = new WeakMap(), _B_instances = new WeakSet(), _B_m = function _B_m() { };
class C {
    constructor() {
        _C_instances.add(this);
    }
}
_C_instances = new WeakSet(), _C_q = function _C_q() { };
C.s = 1;
let D = (_b = class {
        static g() { return __classPrivateFieldGet(D, _b, "f", _D_w); }
    },
    __setFunctionName(_b, "D"),
    _D_w = { value: 1 },
    _b);
class P {
    constructor() {
        _P_instances.add(this);
        _P_x.set(this, 1);
        this[_d] = 2;
    }
    run(o) {
        __classPrivateFieldSet(this, _P_instances, 2, "a");
        __classPrivateFieldGet(o, _P_x, "f").call(o);
        __classPrivateFieldSet(_c, _c, __classPrivateFieldGet(_c, _c, "a", _P_sg_get), "a", _P_sg_set);
        return __classPrivateFieldIn(_P_instances, o);
    }
}
_c = P, _P_x = new WeakMap(), _P_instances = new WeakSet(), _P_m = function _P_m() { return 1; }, _d = k, _P_g_get = function _P_g_get() { return 1; }, _P_sg_get = function _P_sg_get() { return 2; }, _P_sg_set = function _P_sg_set(v) { };
_P_s = { value: void 0 };
P.t = _c.name;
class Q {
    constructor() {
        _Q_z.set(this, void 0);
    }
}
_Q_z = new WeakMap();
class A4 {
    constructor() {
        _A4_foo.set(this, 1);
    }
}
_A4_foo = new WeakMap();
A4.inst = new A4();
class B4 {
}
B4.inst = new B4();
class C4 {
    constructor() {
        _C4_foo.set(this, 1);
    }
}
_e = C4, _C4_foo = new WeakMap();
C4.inst = _e;
class D4 {
    constructor() {
        _D4_instances.add(this);
    }
}
_D4_instances = new WeakSet(), _D4_m = function _D4_m() { };
D4.inst = new D4();
class E4 {
}
_f = E4;
_E4_s = { value: 1 };
E4.inst = new _f();
