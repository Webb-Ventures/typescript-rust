// @module: commonjs
// @target: es2022
// `export =` of an interface emits nothing.
interface Shape {
    x: number;
}
export = Shape;
