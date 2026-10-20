// @target: es2015
// @module: esnext
// @useDefineForClassFields: true
declare let k: string;
class A {
    x;
    y = 1;
    static s;
    static t = this;
    ["q"] = 2;
    [k] = 3;
    0 = 4;
    "str" = 5;
    constructor(public p: number, private r: string) {
        console.log(p);
    }
}
class B extends A {
    z = 1;
}
class C extends A {
    w = 2;
    constructor() {
        console.log(1);
        super(1, "");
        console.log(2);
    }
}
const D = class {
    a = 1;
    static b = 2;
};
