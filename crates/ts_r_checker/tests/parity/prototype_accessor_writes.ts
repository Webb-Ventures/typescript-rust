function C(a: any) {}
C.prototype = {};
class A {}
A.prototype = new A();
const k = Symbol();
const g = Symbol();
interface Foo {
    get [k](): Set<string>;
    set [k](v: string[] | Set<string>);
    get [g](): number;
}
declare const foo: Foo;
foo[k] = ["a"];
foo[k] = 1;
foo[g] = 2;
class D {
    get [k](): number { return 1; }
    set [k](v: number | string) {}
}
new D()[k] = "s";
export {};
