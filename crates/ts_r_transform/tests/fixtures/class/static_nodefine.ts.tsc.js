var _a;
// @target: es2022
// @useDefineForClassFields: false
export class A {
    constructor() {
        this.x = 1;
    }
    static { _a = Symbol.iterator; }
    m() { }
    static { this.s = 2; }
    static #q = 3;
    static { console.log(1); }
    static { this.t = A.s; }
    static { this[_a] = 5; }
}
