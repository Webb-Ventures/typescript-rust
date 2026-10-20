class Base { constructor(...a: any[]) { } }
class A extends Base {
    constructor() {
        const f = () => super();
    }
}
class B extends Base {
    constructor() {
        function g() { }
    }
}
class C extends null {
    constructor() { }
}
declare class D extends Base {
    constructor();
}
const E = class extends Base {
    constructor() { }
};
class F extends Base {
    constructor();
    constructor(x?: number) { }
}
class G extends (Base) {
    constructor() { }
}
class H extends Base {
    constructor() {
        class I extends Base { constructor() { super(); } }
    }
}
interface J { }
class K implements J {
    constructor() { }
}
