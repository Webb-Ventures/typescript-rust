// @target: es2022
declare const dec: any;

@dec
class C {
    static #count = 0;
    static #next() { this.#total += 1; return C.#total; }
    static get #value() { return C.#count; }
    static set #value(v: number) { C.#count = v; }
    static #total = 1;
    static bump() {
        this.#count += 2;
        C.#value = C.#next();
        return #count in C ? C.#value : 0;
    }
}
