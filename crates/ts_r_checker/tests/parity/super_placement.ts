class Base { constructor(...a: any[]) { } m() { return 1; } static s() { } }
class NoBase {
    constructor() {
        super();
        super.m();
    }
    m() { super.m(); }
    x = super.x;
    static y = super.y;
}
class D extends Base {
    constructor(a = super.m(), b = super()) {
        super();
        const f = () => super();
        function g() { super(); }
        const h = () => super.m();
    }
    m() { super(); return super.m(); }
    get p() { return super.m(); }
    x = super.m();
    static z = super.s();
    static { super.s(); }
    q = () => super.m();
    r = function () { return super.m(); };
}
function f() { super(); super.x; }
super.y;
const o = {
    m() { return super.toString(); },
    p: function () { return super.toString(); },
    q: () => super.toString(),
    get r() { return super.toString(); },
};
namespace N { super.x; }
class E extends Base {
    constructor() {
        super();
        class Inner { m() { super.m(); } }
    }
}
