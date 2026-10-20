var o = {};
o.a = 1;
o.a = 2;
function f() {
    o.inner = 1;
    const local = {};
    local.p = 1;
}
var lit = { x: 1 };
lit.y = 2;
class C {
    declared = 1;
    constructor() {
        this.declared = 2;
        this.p = 1;
        const g = () => { this.q = 2; };
        function h() { this.notMine = 3; }
    }
    m() {
        this.p = 3;
    }
    static s() {
        this.st = 1;
    }
    static {
        this.blk = 1;
    }
}
class K {}
K.s = 1;
var KE = class {};
KE.t = 2;
