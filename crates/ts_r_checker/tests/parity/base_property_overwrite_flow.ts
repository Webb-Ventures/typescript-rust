// TS2612 under useDefineForClassFields: a redeclared base field is fine when the
// constructor assigns it on every path (flow), never with `!`, and never reported for
// abstract base fields.
class A { p: number = 1; q: string = ""; r?: number; s: number = 1; t: number = 1; u: number = 1; v?: number; w = 1; }
class B extends A {
    p: number;
    q!: string;
    r?: number;
    declare s: number;
    t: 1;
    u: number;
    v?: number;
    w: number;
    constructor() {
        super();
        if (Math.random()) this.u = 1;
        this.v = 1;
        this.w = 2;
        this.q = "";
    }
}
class E { get g() { return 1; } }
class F extends E { g: number; }
abstract class G { abstract h: number; }
class H extends G { h: number; }
abstract class G2 { abstract h: number; }
class H2 extends G2 { h: number; constructor() { super(); this.h = 1; } }
