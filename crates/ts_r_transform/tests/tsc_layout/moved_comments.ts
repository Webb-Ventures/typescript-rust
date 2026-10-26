// @target: es2015
// @module: esnext
enum E {
    A /* before the comma */,
    B, // after the comma: dropped
    /** leads C */
    C // last member
}
class S {
    static x = 1; // static field
    static {
        // inside a static block
        log(S.x);
    }
    #p = "p"; // private field
    constructor() {
        log(this.#p);
    }
}
/** first line has trailing spaces   
 * and more   
 */
function f() {
    if (S) { // after the brace
    }
    try {
        log(1);
        /* before the brace */ }
    finally { }
    return this.
        f().
        g();
}
declare function log(x: unknown): void;
export { E, S, f };
