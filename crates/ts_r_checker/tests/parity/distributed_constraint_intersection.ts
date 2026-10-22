// A type parameter with a union constraint is assignable to that union intersected with
// itself (`A & T` distributes to `(1 & T) | (2 & T)`).
type A = 1 | 2;
type B = 2 | 3;
function f<T extends A>(a: T): A & T { return a; }
function f2<T extends A, U extends B>(ab: T & U): (A | B) & T & U { return ab; }
function k<T extends string | number>(a: T): (string & T) | (number & T) { return a; }
