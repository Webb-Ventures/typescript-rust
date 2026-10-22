// A getter without annotation returns its setter's annotated parameter type: a reachable
// end is TS2366 (or TS2355 with no `return`) unless that type takes `undefined`.
declare const flip: boolean;
class C {
    get a() {
        if (flip) {
            return 0;
        }
    }
    set a(value: number) {}

    get b() {
        if (flip) {
            return 0;
        }
    }
    set b(value: number | undefined) {}

    get c() {
        throw new Error();
    }
    set c(value: number) {}

    get d() {
        if (flip) {
            return "";
        }
    }
    set d(value) {}
}
const o = {
    get e() {
        if (flip) {
            return 0;
        }
    },
    set e(value: number) {},
    get f() {},
    set f(value: string) {},
};
export {};
