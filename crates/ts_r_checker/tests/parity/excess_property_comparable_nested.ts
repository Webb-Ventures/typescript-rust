// Excess properties: combined index signatures of intersections, fresh case clauses and
// equality operands, nested arrays of a recursive union.
export {};
let x: { [x: string]: { a: 0 } } & { [x: string]: { b: 0 } };
x = { y: { a: 0 } };
x = { y: { a: 0, b: 0 } };
x = { y: { a: 0, b: 0, c: 0 } };
class C { id: number = 1; }
class D extends C { name: string = "" }
switch (new C()) {
    case new D():
    case { id: 12, name: '' }:
    case new C():
}
declare const c: C;
if (c === { id: 12, name: '' }) {}
type Style = StyleBase | StyleArray;
interface StyleArray extends Array<Style> {}
interface StyleBase {
    foo: string;
}
const blah: Style = [
    [[{
        foo: 'asdf',
        jj: 1
    }]]
];
const blah2: Style = [{ foo: 'a', kk: 1 }];
const b1: Style = [[{ foo: 'a', k1: 1 }]];
const b2: Style = [[[{ foo: 'a', k2: 1 }]]];
const b3: StyleArray = [[{ foo: 'a', k3: 1 }]];
