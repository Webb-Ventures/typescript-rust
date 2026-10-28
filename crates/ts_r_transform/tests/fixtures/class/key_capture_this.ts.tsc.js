"use strict";
var _a, _b, _c, _d, _e;
class C {
    static { this.c = "foo"; }
    static { this.bar = (_c = () => { _a = this.c, _b = this.c; },
        class Inner {
                constructor() {
                    this[_b] = 123;
                }
                static { _c(); }
                static { this[_a] = 123; }
            }); }
    static {
        _e = () => { _d = this.c; };
        class D {
            constructor() {
                this[_d] = 1;
            }
            static { _e(); }
        }
    }
}
