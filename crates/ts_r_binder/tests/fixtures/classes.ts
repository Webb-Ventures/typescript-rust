// Class members: per-declaration member tables, statics, accessors, private names.
class A {
    x = 1;
    static x = 2;
    get y() { return 1; }
    set y(v) {}
    #p = 3;
    m() {}
    m(): void;
    dupe = 1;
    dupe = 2;
    static prototype() {}
    constructor(public readonly param: number) {}
}
const Expr = class Named { self = Named; };
abstract class B { abstract z: number; }
