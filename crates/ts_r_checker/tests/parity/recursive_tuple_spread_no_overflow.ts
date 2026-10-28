type UA = readonly unknown[];
type T1<A extends UA> = A extends UA ? (A extends readonly [unknown?, ...infer R] ? R : []) extends infer Res ? Res : never : never;
type M1<T extends UA, X> = T extends [] ? T : [X, ...M1<T1<T>, X>];
type T2<A extends UA> = A extends UA ? (A extends readonly [unknown?, ...infer R] ? R : []) : never;
type M2<T extends UA, X> = T extends [] ? T : [X, ...M2<T2<T>, X>];
type T4<A extends UA> = {a: 1} extends infer O extends {a: 1} ? A extends UA ? A : never : never;
type M4<T extends UA, X> = T extends [] ? T : [X, ...M4<T4<T>, X>];
