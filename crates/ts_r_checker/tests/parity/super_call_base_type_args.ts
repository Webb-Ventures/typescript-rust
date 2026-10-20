// `super(...)` takes the base constructor as the `extends` clause's type arguments
// instantiate it.
export {};
interface ContextualType<T> {
    method(parameter: T): void;
}
class CBase<T> {
    constructor(param: ContextualType<T>) {}
}
class C extends CBase<string> {
    constructor() {
        super({
            method(p) {
                p.length;
            }
        });
    }
}
class B2<T> {
    constructor(x: T) {}
}
class D2 extends B2<number> {
    constructor() {
        super("no");
    }
}
