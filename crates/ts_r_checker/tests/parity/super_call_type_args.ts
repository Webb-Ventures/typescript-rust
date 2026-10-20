// `super<T>(...)` is TS2754 (tsc then reports no other semantic error in the program).
export {};
class B2<T> {
    constructor(x: T) {}
}
class D3<T> extends B2<T> {
    constructor(x: T) {
        super<T>(x);
    }
}
