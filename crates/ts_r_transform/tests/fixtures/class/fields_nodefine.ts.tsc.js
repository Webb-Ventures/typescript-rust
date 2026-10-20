"use strict";
class A extends Base {
    static { this.s = 2; }
    #p;
    #acc_accessor_storage;
    get acc() { return this.#acc_accessor_storage; }
    set acc(value) { this.#acc_accessor_storage = value; }
    constructor(x, y) {
        "use strict";
        console.log("pre");
        super(x);
        this.x = x;
        this.y = y;
        this.a = 1;
        this.#p = 3;
        this["comp"] = 4;
        this.#acc_accessor_storage = 5;
        this.a = 2;
    }
    m() { }
}
class B extends Base {
    constructor() {
        super(...arguments);
        this.f = 1;
    }
}
class C {
    constructor(x) {
        this.x = x;
        this.g = this.x;
    }
}
class D extends Base {
    constructor(x) {
        this.x = x;
        if (x) {
            super();
        }
        else {
            super(1);
        }
    }
}
const E = class {
    constructor(z) {
        this.z = z;
    }
};
