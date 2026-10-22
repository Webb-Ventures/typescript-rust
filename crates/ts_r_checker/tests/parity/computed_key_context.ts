// An object literal member with a computed key that is not a literal is contextually typed
// by the index signatures for its key type: a string key by the string index signature, a
// number key by the number one when there is one, else by the string one.
interface S {
    [s: string]: (x: string) => number;
    [s: number]: (x: any) => number;
}
interface N {
    [s: string]: (x: any) => number;
    [s: number]: (x: string) => number;
}
interface OnlyString {
    [s: string]: (x: string) => number;
}
declare const k: string;
export const s: S = {
    ["" + 0](y) { return y.length; },
    [k]: y => y.length,
};
export const n: N = {
    [+"foo"](y) { return y.length; },
    [+"bar"]: y => y.length,
};
export const o: OnlyString = {
    [+"foo"](y) { return y.length; },
    [+"bar"]: y => y.length,
};
export const none: { a: (x: string) => void } = { [k]: y => y };
