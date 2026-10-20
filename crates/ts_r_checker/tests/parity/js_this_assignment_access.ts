// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext"}, "files": ["a.js", "b.js"]}
// @filename: a.js
// `@private`/`@protected` on a `this.x = v` assignment make the property private or
// protected, as on a class member.
export class C {
    constructor() {
        /**
         * Docs.
         *
         * @private
         */
        this.priv = 1;
        /** @protected */
        this.prot = 2;
        /** @public */
        this.pub = 3;
        /** plain */
        this.plain = 4;
    }
    init() {
        /** @private */
        this.late = 5;
    }
    h() { return this.priv + this.prot + this.late; }
}
export class D extends C {
    n() {
        return this.priv + this.prot + this.pub + this.late;
    }
}
const c = new C();
c.priv + c.prot + c.pub + c.plain + c.late;
new D().prot;
// @filename: b.js
// A `this.x` assigned only in methods (not the constructor) is `T | undefined` under
// strictNullChecks; `this.x` is not an entity name for TS2532.
export class E {
    constructor() {
        this.both = 1;
    }
    m() {
        this.both = 2;
        this.only = "s";
        return this.only.length;
    }
    n() {
        return this.both + this.only.length;
    }
}
export class Base {
    constructor() {
        this.p = 1;
    }
}
export class Derived extends Base {
    m() {
        this.p = 1;
        return this.p + 1;
    }
}
new E().only.length;
