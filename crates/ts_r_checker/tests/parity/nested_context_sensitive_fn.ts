// A function whose body is a context-sensitive function is context sensitive, so `T` is
// fixed (and widened) before the inner parameter is typed; a computed entity-name key in a
// binding pattern prints as written.
declare var f10: <T>(x: T, b: () => (a: T) => void, y: T) => T;
f10('', () => a => a.foo, '');
const fn = (param: string) => undefined;
const foo = { bar: 'a' };
fn(({ [foo.bar]: c }) => undefined);
