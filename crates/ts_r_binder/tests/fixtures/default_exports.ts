// `export default <expr>` conflicts with defaults bound before it; a later class does not
// (the checker reports TS2323 there).
const foo = 1;
interface I {}
export default foo;
export default class Foo {}
export default I;
