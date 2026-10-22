// A generic string mapping target accepts the same mapping over a related type, or a
// source the mapping leaves unchanged that fits the inner type; `string` fits neither.
function f<T extends string, U extends T>(s: string, x: Uppercase<T>, y: Uppercase<U>) {
    x = s;
    y = s;
    y = x;
    x = y;
    const l: Lowercase<T> = x;
    const n: number = x;
}
function g<T extends string>(x: Uppercase<T>, v: Uppercase<string>) {
    let w: Uppercase<string> = x;
    x = v;
    const k: Uppercase<T> = "ABC";
}
