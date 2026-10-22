declare global {
    namespace globalThis {
        var test: string;
    }
}
globalThis.tests = "a-b";
globalThis.test.split("-");

class Box {
    items: string[] = [];
    copy(other: string[]) {
        for (var k in this.items) {
            other[k] = this.items[k];
        }
        for (const j in other) {
            this.items[j] = other[j];
        }
    }
}

function base<T>() {
    class Base {
        static prop: T;
    }
    return Base;
}
class Gen<T> extends base<T>() {}
class Ok<T> extends Array<T> {}
export { Gen, Ok };
