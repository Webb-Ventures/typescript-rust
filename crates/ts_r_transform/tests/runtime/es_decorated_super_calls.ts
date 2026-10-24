function dec(value: any) { return value; }
let keys = 0;
let args = 0;
let reads = 0;
function key() { keys++; return "m"; }
function arg() { args++; return 1; }
class Base {
    static get m() {
        reads++;
        return function (n: number) { return this.tag + n; };
    }
    static missing: any = undefined;
    static nil: any = null;
}
@dec
class Derived extends Base {
    static tag = 10;
    static optional = super.m?.(arg());
    static paren = (super.m)(arg());
    static computed = ((super[key()]))?.(arg());
    static computedParen = ((super[key()]))(arg());
    static absent = super.missing?.(arg());
    static nullish = (super.nil)?.(arg());
    static {
        if (super.m?.(arg()) !== 11 || (super[key()])(arg()) !== 11) {
            throw new Error("static block receiver");
        }
    }
}
if (Derived.optional !== 11 || Derived.paren !== 11 ||
    Derived.computed !== 11 || Derived.computedParen !== 11 ||
    Derived.absent !== undefined || Derived.nullish !== undefined ||
    keys !== 3 || args !== 6 || reads !== 6) {
    throw new Error("super call receiver or evaluation order");
}
console.log("super calls ok");
