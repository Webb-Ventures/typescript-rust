// Private-name grammar: TS18024 (enum member), TS18012 (#constructor), TS18011 (delete).
enum E { #x, y }
class A { #v = 1; #constructor() {} get #constructor() { return 1 } m() { delete this.#v; delete (this.#v); delete this.#v.x; } }
class B { #constructor = 1; static #constructor2 = 1 }
class C { static #constructor() {} }
