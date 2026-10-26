// @target: es2022
declare var f: any;
class C1 {
    accessor ["w"]: any;
    static accessor ["z"] = 2;
}
class C2 {
    #a1_accessor_storage = 1;
    accessor a1 = 2;
    accessor [f()] = 1;
}
class C4_1 {
    static accessor a3 = 1;
}
class C4_2 {
    static accessor a3 = 1;
}
