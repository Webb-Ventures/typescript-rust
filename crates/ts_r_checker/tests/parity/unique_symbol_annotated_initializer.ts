// Symbol() initializing a const or static readonly property is its unique symbol,
// annotated or not.
export const animationFrames: unique symbol = Symbol('animationFrames');
const b: unique symbol = Symbol.for('x');
export const c: unique symbol = (Symbol('y'));
class K { static readonly s: unique symbol = Symbol(); }
let m: symbol = Symbol();
