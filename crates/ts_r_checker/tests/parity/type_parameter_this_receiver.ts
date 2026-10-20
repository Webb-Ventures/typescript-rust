// Members of a type parameter constrained to a class or interface see the type parameter as this.
export {};
class A {
    self() {
        return this;
    }
    me!: this;
}
function f<T extends A>(x: T) {
    x = x.self();
    x = x.me;
    const y: T = x.self();
    return y;
}
interface HasSelf { self(): this }
function g<T extends HasSelf>(x: T) {
    x = x.self();
}
function h<T extends A & { z: 1 }>(x: T) {
    x = x.self();
}
