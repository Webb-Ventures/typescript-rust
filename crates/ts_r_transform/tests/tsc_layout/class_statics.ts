// @target: es2015
// @module: esnext
declare let k: string;
class A {
    static s = 1;
    static t;
    x = 2;
    static {
        console.log(this.s);
    }
    static [k] = 3;
    static m() { return A.s; }
}
const B = class {
    static s = 1;
    static { this.s++; }
};
class C extends A {
    static u = super.m();
    static {
        super.m();
    }
}
class D {
    static self = D;
    static f = () => this;
}
export class E {
    static s = 1;
}
export default class {
    static s = 1;
}
declare let a: any;
function f() { return class { static s = this; } }
a.x?.b;
function g() { return a.y?.b; }
function h() { return class { static s = this; } }
function k() { var x = a.y?.b; return class { static s = this; } }
