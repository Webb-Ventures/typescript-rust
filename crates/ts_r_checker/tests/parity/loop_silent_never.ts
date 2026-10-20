// A loop's incomplete type narrowed to never is silent (no error, adds nothing); `object`
// narrowed by typeof "function" is `Function`.
declare var cond: boolean;
export function foo2() {
    let x: string | number | boolean = 0;
    while (cond) {
        if (typeof x === "number") {
            x = "abc";
        }
        else {
            x = x.slice();
        }
    }
    return x;
}
export function foo3(instance: Function | object) {
    if (typeof instance === 'function') {
        if (instance.prototype == null || instance.prototype.constructor == null) {
            return instance.length;
        }
    }
    return 0;
}
