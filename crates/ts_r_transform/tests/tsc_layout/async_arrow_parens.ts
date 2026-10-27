// @target: es2015
// A lowered async arrow's lone parameter is parenthesized.
declare function g(x: number): Promise<number>;
const x = async i => await g(i);
