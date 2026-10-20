class D {
    static C1 = { get [D.D]() { return 0; } };
    static C2 = { ...{ get [D.D]() { return 0; } } };
    static C3 = { m: { get [D.D]() { return 0; } } };
    static C4 = { [D.D]() { return 0; } };
    static C5 = { ...{ [D.D]: 1 } };
    static D = '';
}
class E {
    a = { get [this.x]() { return 0; } };
    b = { ...{ get [this.x]() { return 0; } } };
    c = { ...{ [this.x]: 1 } };
    x = '';
}
class Base { b = 1; a = 1; c = 1; e = 1; }
class Derived extends Base {
    b;
    d = this.b;
    f = this.c;
    c = 2;
    g = this.a;
    e!: number;
    h = this.e;
    constructor(public a: number) { super(); }
}
export {};
