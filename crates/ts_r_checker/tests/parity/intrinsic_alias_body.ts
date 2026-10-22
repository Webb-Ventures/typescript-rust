// `intrinsic` as a type alias body is only valid for the compiler-provided names with the
// right arity; anywhere else it is TS2795.
type Uppercase<S extends string> = intrinsic;
type Foo<T> = intrinsic;
type Bar = intrinsic;
type Capitalize<A, B> = intrinsic;
