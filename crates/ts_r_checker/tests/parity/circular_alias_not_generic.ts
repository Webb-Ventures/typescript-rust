type A<T> = A<T>;
type B<T, U> = B;
type C<T> = C<T>[] | T;
type D<T> = { x: D };
export type { A, B, C, D };
