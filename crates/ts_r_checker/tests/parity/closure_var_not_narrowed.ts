// A closure sees a `let` or parameter narrowed past its last assignment, not a `var`.
function f13() {
    var foo: string | undefined;
    foo = '';
    return () => {
        foo.toLocaleLowerCase();
    }
}
function f14() {
    let foo: string | undefined;
    foo = '';
    return () => {
        foo.toLocaleLowerCase();
    }
}
function f15(foo: string | undefined) {
    foo = '';
    return () => {
        foo.toLocaleLowerCase();
    }
}
function f16() {
    var { a }: { a: string | undefined } = { a: undefined };
    a = '';
    return () => a.length;
}
