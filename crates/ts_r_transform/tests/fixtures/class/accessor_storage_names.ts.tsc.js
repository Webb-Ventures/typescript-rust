"use strict";
var _a;
class C1 {
    #_a_accessor_storage;
    get ["w"]() { return this.#_a_accessor_storage; }
    set ["w"](value) { this.#_a_accessor_storage = value; }
    static #_b_accessor_storage = 2;
    static get ["z"]() { return C1.#_b_accessor_storage; }
    static set ["z"](value) { C1.#_b_accessor_storage = value; }
}
class C2 {
    #a1_accessor_storage = 1;
    #a1_1_accessor_storage = 2;
    get a1() { return this.#a1_1_accessor_storage; }
    set a1(value) { this.#a1_1_accessor_storage = value; }
    #_a_accessor_storage = 1;
    get [_a = f()]() { return this.#_a_accessor_storage; }
    set [_a](value) { this.#_a_accessor_storage = value; }
}
class C4_1 {
    static #a3_accessor_storage = 1;
    static get a3() { return C4_1.#a3_accessor_storage; }
    static set a3(value) { C4_1.#a3_accessor_storage = value; }
}
class C4_2 {
    static #a3_accessor_storage = 1;
    static get a3() { return C4_2.#a3_accessor_storage; }
    static set a3(value) { C4_2.#a3_accessor_storage = value; }
}
