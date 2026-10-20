// @target: es2022
// @useDefineForClassFields: false
declare const dec: any;
const key = "k";

class Fields {
    @dec a = 1;
    @dec [key] = 2;
    b = 3;
    @dec static s = 4;
    @dec m() {}
    constructor(public p: number) {}
}

class Derived extends Fields {
    @dec c = 5;
}
