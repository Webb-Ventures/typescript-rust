abstract class A1 {
    a = this.p;
    abstract p: string;
    b = this.p;
    abstract q?: string;
    c = this.q;
    abstract accessor r: string;
    d = this.r;
}
abstract class A2 {
    abstract p: string;
    static s = 1;
}
abstract class A3 {
    x = this.p;
    abstract p: string;
}
abstract class A4 {
    abstract p: string;
    x = this.p;
}
class B1 {
    o = {
        get [this.o2]() { return 1; },
        set [this.o3](v: any) {},
        [this.o4]() {},
    };
    o2: any;
    o3: any = 1;
    o4: any;
}
export {};
