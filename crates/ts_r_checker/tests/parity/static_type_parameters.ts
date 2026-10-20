function foo<T>() { return '' }
class C<T> {
    static a = (n: T) => { };
    static b: T;
    static c: T[] = [];
    static f(xs: T[]): T[] { return xs; }
    static get g(): T { return null!; }
    static [x: string]: unknown;
    static {
        let v: T;
    }
    static bar() {
        var obj = {
            [foo<T>()]() { }
        };
        class D { x!: T; }
        function h<T>(y: T) { }
        return 0;
    }
    x!: T;
    m(): T { return null!; }
    static q: typeof C.prototype;
    static r = class<U> { u!: U; t!: T };
}
const E = class<T> {
    static z: T;
};
interface I<T> { }
class F<T> {
    static k: I<T>;
}
