// @target: es2022
// @noEmitHelpers: true
declare var dec: any;
declare class Base {
    static m(...args: any[]): number;
    static x: number;
}
const k = "m";

@dec
class A extends Base {
    static a = super.m(1);
    static b = super[k]``;
    static {
        super.x;
        (() => super[k](this))();
        function f() { return super.x; }
    }
    m() { return super.m(); }
}

@dec
class B extends class { } {
    static {}
}

@dec
class C extends (function () { } as any) {
    static { this; }
}
