/**
 * @param {String} a
 * @param {Box} b
 * @param {Pair<string>} c
 * @param {number=} d
 * @param {*} e
 * @param {?} f
 * @param {function(string): number} g
 */
export function f(a, b, c, d, e, f, g) {}

/** @param {? | string} x */
export function nullable(x) {}

/** @this {string} */
export function withThis() {}

export class Both {
    size() { return 1; }
    draw() { return 2; }
}

export class Point {
    static {
        this.origin = 0;
    }
    constructor() {
        /** The x coordinate. */
        this.x = 1;
        this.y = "y";
    }
    move() {
        this.moved = true;
        this.x = 2;
    }
}

export function g() {}
g.undefined = 1;
g.y = 2;

