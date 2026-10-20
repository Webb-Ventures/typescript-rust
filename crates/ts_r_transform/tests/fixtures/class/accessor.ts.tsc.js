var _a;
// @target: es2022
export class A {
    static #sa_accessor_storage = 1;
    static get sa() { return A.#sa_accessor_storage; }
    static set sa(value) { A.#sa_accessor_storage = value; }
    #pa_accessor_storage = 2;
    get #pa() { return this.#pa_accessor_storage; }
    set #pa(value) { this.#pa_accessor_storage = value; }
    #_a_accessor_storage = 3;
    get [_a = "c" + 1]() { return this.#_a_accessor_storage; }
    set [_a](value) { this.#_a_accessor_storage = value; }
    #_b_accessor_storage = 4;
    get "str"() { return this.#_b_accessor_storage; }
    set "str"(value) { this.#_b_accessor_storage = value; }
    #_c_accessor_storage = 5;
    get 5() { return this.#_c_accessor_storage; }
    set 5(value) { this.#_c_accessor_storage = value; }
    #x_accessor_storage;
    get x() { return this.#x_accessor_storage; }
    set x(value) { this.#x_accessor_storage = value; }
}
export default class {
    q;
    constructor(q) {
        this.q = q;
    }
}
