// @target: es2022
declare const dec: any;
declare class Base { static x: any; }
const k = "x";

@dec
class C extends Base {
    static a = super.x ??= 1;
    static b = () => super[k] += 2;
    static c = () => { super.x++; };
    static d = (super.x = 1, super.x);
    static {
        const v = super[k]--;
        super.x ||= v;
        for (super.x = 0; super.x < 3; super.x++) {}
        for (super.x of [1]) {}
    }
}
