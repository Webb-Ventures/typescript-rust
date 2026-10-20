declare function f<T>(value: T): void;
declare function fs(value: string): void;
declare function fa(value: any): void;
declare function fu(value: unknown): void;
declare function fo(value: string): void;
declare function fo(value: number): void;
declare function fc<T extends string>(value: T): void;
function* g1() {
    const a = yield;
    f(yield);
    fs(yield);
    fa(yield);
    fu(yield);
    fo(yield);
    fc(yield);
    const b = (yield) as string;
    const c = <number>(yield);
    const d = [yield];
    const e = { p: yield };
    if (yield) { }
    while (yield) { }
    const h = yield* [1];
    return yield;
}
const g2: () => Generator<number, void, string> = function* () {
    const a = yield 1;
};
function* g3(): Generator<number, void, string> {
    const a = yield 1;
}
function* g4(): any {
    const a = yield 1;
}
function* g5() {
    let x;
    x = yield;
    let y: any;
    y = yield;
    yield yield;
    (yield) + 1;
    !(yield);
    typeof (yield);
    const z = (yield, 1);
    const q = `${yield}`;
    return;
}
const o = {
    *m() {
        const a = yield;
    }
};
class C {
    *m() {
        const a = yield;
    }
}
async function* ag() {
    const a = yield;
    const b = await (yield);
}
