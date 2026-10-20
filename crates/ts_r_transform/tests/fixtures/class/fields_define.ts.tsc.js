"use strict";
class A extends Base {
    x;
    y;
    a = 1;
    b;
    static s = 2;
    static t;
    #p = 3;
    ["comp"] = 4;
    #acc_accessor_storage = 5;
    get acc() { return this.#acc_accessor_storage; }
    set acc(value) { this.#acc_accessor_storage = value; }
    constructor(x, y) {
        "use strict";
        console.log("pre");
        super(x);
        this.x = x;
        this.y = y;
        this.a = 2;
    }
    m() { }
}
class B extends Base {
    f = 1;
}
class C {
    x;
    constructor(x) {
        this.x = x;
    }
    g = this.x;
}
class D extends Base {
    x;
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
    z;
    constructor(z) {
        this.z = z;
    }
};
