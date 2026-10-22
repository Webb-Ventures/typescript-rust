declare function foo3<T extends string>(a: T, b: NoInfer<T[]>): T;
declare function foo5<T extends string>(a: T, b: NoInfer<{ x: T }>): T;
const r3 = foo3('foo', ['bar']);
const r5 = foo5('foo', { x: 'bar' });
const c3: number = r3;
const c5: number = r5;
export {};
