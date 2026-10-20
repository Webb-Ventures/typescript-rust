// @target: es2015
// @module: esnext
// @noEmitHelpers: true
class A {
    #x = 1;
    #f = () => 1;
    a!: A;
    m(o: A) {
        this.a.#x += 1;
        o.#x *= 2;
        this.a.#x = 3;
        this.a.#x++;
        const r = o.#x--;
        this.#f();
        this.a.#f(1, 2);
        o.#f``;
        return this.a.#x;
    }
}
foo(class { #y = 1; static #z = 2; get() { return this.#y; } });
class B extends A {
    #b = 1;
    #m() { }
}
class C {
    #q() { }
    static s = 1;
}
let D = class { static #w = 1; static g() { return D.#w; } };
declare const k: string;
class P {
    #m() { return 1; }
    #x = 1;
    [k] = 2;
    static #s: number;
    static t = this.name;
    get #g() { return 1; }
    static get #sg() { return 2; }
    static set #sg(v) { }
    run(o: P) {
        this.#g = 2;
        o.#x();
        P.#sg = P.#sg;
        return #g in o;
    }
}
class Q {
    #z: number;
}
class A4 {
    #foo = 1;
    static inst = new A4();
}
class B4 {
    static inst = new B4();
}
class C4 {
    #foo = 1;
    static inst = this;
}
class D4 {
    #m() { }
    static inst = new D4();
}
class E4 {
    static #s = 1;
    static inst = new E4();
}
