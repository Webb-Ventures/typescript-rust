interface A<T> { a: T }
interface A<U> { b: U }
interface B<T> { a: T }
interface B<T, U> { b: U }
interface C<T extends string> { a: T }
interface C<T extends number> { b: T }
interface D<T = string> { a: T }
interface D<T> { b: T }
interface E<T extends string> { a: T }
interface E<T> { b: T }
interface F<T> { a: T }
interface F<T> { b: T }
interface F<T> { c: T }
interface G { a: 1 }
interface G<T> { b: T }
class H<T> { }
interface H<U> { }
interface J<in out T> { a: T }
interface J<T> { b: T }
interface K<T = number> { a: T }
interface K<T = string> { b: T }
interface L {}
interface L<T> {}
interface L<T = number, U = string> {}
export {};
