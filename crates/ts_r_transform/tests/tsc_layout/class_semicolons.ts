// @target: es2015
// @module: esnext
class A {
    ;
}
class B {
    #m() { return 1; }; // after a lowered private method
    x = 1;
    ;
    get y() { return this.#m(); };
    z = class { ; };
}
export { A, B };
