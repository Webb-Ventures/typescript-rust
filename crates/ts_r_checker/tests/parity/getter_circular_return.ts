// A getter without a return type annotation whose inferred type needs its own property
// type (no setter annotation types it) is TS7023 at its name.
class D {
    get x() {
        return this.x;
    }
}
class E {
    get x() { return this.y; }
    get y() { return this.x; }
}
class G {
    get x() { return this.x; }
    set x(v) { }
}
class H {
    get x() { return this.x + 1; }
    set x(v: number) { }
}
class I {
    get x() { return this.p; }
    p = this.x;
}
class J {
    static get x() { return J.x; }
    get y() { return [this.y]; }
}
export {};
