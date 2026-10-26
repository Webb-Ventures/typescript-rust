// @target: es2022
// @experimentalDecorators: true
declare var dec: any;
@dec
class C1 {
    static instance = new C1();
}
@dec
class C2 {
    static {
        new C2();
    }
}
@dec
class C3 {
    m() { return C3; }
}
