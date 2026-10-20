interface IFace {
    member1;
    member2?;
    readonly member3;
    "quoted";
    42;
    [k: string]: any;
    get acc();
    set acc2(v);
}
var x: { y; z; };
declare var objL: { v; w; };
type T = { a; b: number };
declare class D { p; static q; private r; #s; }
declare namespace NS { interface J { j; } }
declare global { interface G { g; } }
function f(o: { b }) {}
interface AccA {
    get a();
    set a(v);
    get b(): number;
    set b(v);
    get c();
    set c(v: number);
    set d(v);
    get e();
}
type AccT = { get f(); set g(v); };
export {};
