// An intersection source related through one member still needs its other members'
// properties to meet the target's.
declare let x: { a?: number, b: string };
declare let z: { a: null } & { b: string };
x = z;
interface To {
    field?: number;
    anotherField: string;
}
type From = { field: null } & Omit<To, 'field'>;
function foo(v: From) {
    let t: To;
    t = v;
}
function bar<T extends object>(x: { a?: string }, y: T & { a: boolean }) {
    x = y;
}
interface Test {
    readonly hi?: string[]
}
function test<T extends object>(value: T): Test {
    return { ...value, hi: true }
}
