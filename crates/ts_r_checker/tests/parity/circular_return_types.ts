// TS7023 at a function whose inferred return type depends on itself through the return
// types of named functions alone. A bare self-call in a return adds nothing to the inferred
// type, so a function returning only that call returns `never`.
function g() {
    return g();
}
var f1 = function () {
    return f1();
};
var f2 = () => f2();
function h() {
    return foo();
    function foo() {
        return h() || "hello";
    }
}
function base(n: number) {
    if (n > 0) {
        return base(n - 1);
    }
    return 1;
}
async function later() {
    return await later();
}
function first(arg: Parameters<typeof second>[0]) {
    return arg;
}
function second(arg: string) {
    return first(arg);
}
const never: never = g();
const one: number = base(1);
export {};
