/**
 * @param {String} a
 * @param {Box} b
 * @param {Pair<string>} c
 * @param {number=} d
 * @param {*} e
 * @param {?} f
 * @param {function(string): number} g
 */
export declare function f(a: string, b: Box<any>, c: Pair<string, any>, d?: number | undefined, e: any, f: any | null, g: Function): void;
/** @param {? | string} x */
export declare function nullable(x: (any | null) | string): void;
/** @this {string} */
export declare function withThis(this: string): void;
export declare class Both {
    size(): number;
    draw(): number;
}
export declare class Point {
    static origin: number | undefined;
    /** The x coordinate. */
    x: number;
    y: string;
    moved: boolean | undefined;
    constructor();
    move(): void;
}
export declare function g(): void;
export declare namespace g {
    var _a: number;
    export { _a as undefined };
    export var y: number;
}
