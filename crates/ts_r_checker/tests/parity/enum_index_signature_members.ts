// Members are checked against index signatures whose value is an enum like any other
// (another enum, or `string`, is not assignable; `number` is to a numeric enum).
enum E { A }
enum E2 { A }
interface I14 {
    [x: string]: E2;
    foo: E;
    bar: string | number;
    baz: number;
    qux: E2.A;
}
interface Base { foo: E }
interface I15 extends Base {
    [x: string]: E2;
}
interface I16 {
    [x: string]: E;
    s: string;
}
