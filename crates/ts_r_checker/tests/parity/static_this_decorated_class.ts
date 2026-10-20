// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "experimentalDecorators": true, "noEmit": true}, "files": ["a.ts"]}
// @filename: a.ts
// `this` in a static field value of a class with legacy decorators: TS2816.
declare const foo: any;
@foo
class C {
    static a = 1;
    static b = this.a + 1;
    static f = () => this.a;
    static g = function (this: any) { return this.a; };
    c = this;
    static { this.a; }
    static m() { return this.a; }
}
class E {
    @foo static a = 1;
    static b = this.a + 1;
}
export {};
