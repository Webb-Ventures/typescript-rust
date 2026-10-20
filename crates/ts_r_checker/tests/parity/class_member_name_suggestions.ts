// An unresolved name inside a class member that names a member of the class: TS2662 for
// static members (anywhere in the class), TS2663 for instance members (not in static
// members); inherited members count, arrows are transparent, other functions are not.
class Base { protected v = 1; static s = 2; }
class D extends Base {
    w = 3;
    static t = 4;
    constructor(private p: number) { super(); }
    m() {
        v;
        s;
        w;
        p;
        t;
        const f = () => w;
        function g() { return w; }
        prototype;
        length;
        bind;
        toString;
        hasOwnProperty;
    }
    static n() {
        w;
        t;
    }
    q = () => t;
    static { w; }
}
const K = class { static z = 1; m() { z; } };
const J = class Named { static z = 1; m() { z; } };
export default class { static z = 1; m() { z; } }
