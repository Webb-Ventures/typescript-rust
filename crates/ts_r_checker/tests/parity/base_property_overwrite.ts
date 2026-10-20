class Base {
    b = 1;
    c?: number;
    get g() { return 1; }
    m() {}
    static s = 1;
    declare d: number;
    p: any;
}
class Derived extends Base {
    b!: number;
    c?: number;
    g: number;
    m: () => void;
    static s: number;
    declare b2: number;
    d: number;
    p: string;
    constructor() { super(); this.g = 1; this.m = () => {}; this.d = 1; this.p = ''; }
}
interface I { x: number }
class Impl implements I { x: number = 1; }
class Base2 { a = 1; }
class Mid extends Base2 {}
class Leaf extends Mid { a: number; constructor() { super(); this.a = 2; } }
class Leaf2 extends Mid { declare a: number; }
class Leaf3 extends Mid { a = 3; }
class BaseB { a = 1; }
class MidB extends BaseB {}
class Leaf4 extends MidB { a: number; }
const BE = class { q = 1; };
class FromExpr extends BE { q: number; }
class PP { constructor(public r: number) {} }
class FromPP extends PP { r: number; }
class G<T> { v!: T; }
class H extends G<string> { v: string; }
export {};
