declare const obj: any;
declare function f(): any;
declare var y: any;
declare function foo(): any;
declare var bar: any;
var a: any, b: any, o: any = {};
var x1 = new y = 5;
(foo()) = bar;
foo() = 1;
1 >>= 2;
o.p = 1;
o["q"] = 2;
(o.p as any) = 3;
(o!) = 4;
(<any>o) = 5;
a! += 1;
[a, b] = [1, 2];
({ a } = o);
({ a: 1 } = o);
[""] = [""];
[a.b, o[0], ...b] = [1, 2];
[[a], { b }] = o;
[a = 1, (b)] = o;
for ([""] of [[""]]) { }
for ([a] of [[1]]) { }
class C {
    m() {
        this = 1;
        super.m = 2;
    }
}
let obj2: { fn?: <T>() => T } = {};
obj2.fn<number> = () => 1234;
null = 1;
"s" = 1;
a?.b = 1;
(a, b) = 1;
[a + 1] = [1];
({ a: a + 1 } = o);
({ ...foo() } = o);
[...foo()] = [];
obj?.a++;
--obj?.a.b;
(obj?.a)++;
obj?.a = 1;
obj?.a.b += 1;
(obj?.a).b = 1;
for (obj?.a in {});
for (obj?.a.b of []);
for (f() in {});
for (f() of []);
for ((obj.a) in {});
for ([obj.a] of [[1]]);
({ ...obj?.a } = { a: 1 });
({ ...f() } = { a: 1 });
({ a: obj?.a } = { a: 1 });
[...obj?.a] = [];
null **= 2;
"x" += 1;
1 >>= 2;
