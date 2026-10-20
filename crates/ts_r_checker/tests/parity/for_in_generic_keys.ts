// A for-in variable over a generic value is Extract<keyof T, string>, else string.
function g<T>(o: T) {
    for (var k in o) {
        const n: number = k;
    }
    var k: string;
}
function h(o: object) {
    for (var k in o) { }
    var k: number;
}
class A {
    biz() {
        for (var y in this) { }
        var y: string;
    }
}
type Boxified<T> = { [P in keyof T]: { value: T[P] } };
function boxify<T>(obj: T): Boxified<T> {
    if (typeof obj === "object") {
        const result = {} as Boxified<T>;
        for (const k in obj) {
            result[k] = { value: obj[k] };
        }
        return result;
    }
    return obj as any;
}
// An enum member initializer is compared with its enum type.
enum E {
    a
}
var x = E;
var x = E.a;
export {};
