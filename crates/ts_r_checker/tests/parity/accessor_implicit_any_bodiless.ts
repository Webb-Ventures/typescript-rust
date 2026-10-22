abstract class P {
    abstract get a();
    abstract get b();
    abstract set b(v: number);
    abstract get c();
    abstract set c(v);
}
declare class D {
    get x();
    get y();
    set y(v);
}
interface I {
    get z();
}
type T = { get w(); };
declare class Pv { private get g(); private set h(v); }
export {};
