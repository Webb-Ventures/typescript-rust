declare const decorate: any;
declare const c: boolean;
class Base { constructor(...a: any[]) { } m() { } }
class D1 extends Base {
    constructor() {
        this.m();
    }
}
class D2 extends Base {
    constructor() {
        if (c) super();
        this.m();
    }
}
class D3 extends Base {
    constructor() {
        if (c) { super(); } else { super(1); }
        this.m();
    }
}
class D4 extends Base {
    constructor() {
        super.m();
        super(this);
        super.m();
    }
}
class D5 extends Base {
    constructor() {
        @decorate(this)
        class Inner { }
        (() => this)();
        const f = function () { return this; };
        super();
    }
}
class D6 extends null {
    constructor() {
        this;
    }
}
class D7 extends Base {
    constructor() {
        try { super(); } catch { }
        this.m();
    }
}
class D8 extends Base {
    constructor() {
        for (;;) { this.m(); super(); }
    }
}
class D9 extends Base {
    constructor(public p = this) {
        super();
    }
}
class D10 extends Base {
    constructor() {
        const x = super() || this;
    }
}
class D11 extends Base {
    x = this;
    constructor() {
        super();
        return;
        this;
    }
}
class D12 extends Base {
    constructor() {
        while (c) { super(); }
        this.m();
    }
}
