var a1 = { name: "x", age: 1 };
var b1 = { name: "y", age: 2, dob: new Date() };
let n1: number = [a1, b1];
let n2: number = [{ name: "x", age: 1 }, { name: "y", age: 2, dob: new Date() }];
(function foo(x: number, y?: number) { let q: string = foo; return x })(12);
(function bar(x, y) { let q: string = bar; return x })(12);
