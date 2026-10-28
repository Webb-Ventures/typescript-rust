// @target: es2022
// @useDefineForClassFields: false
declare var k: any;
class C {
    static readonly c: "foo" = "foo";
    static bar = class Inner {
        static [this.c] = 123;
        [this.c] = 123;
    };
    static {
        class D {
            [this.c] = 1;
        }
    }
}
