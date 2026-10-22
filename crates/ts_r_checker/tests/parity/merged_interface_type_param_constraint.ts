// A merged interface's type parameter takes the constraint any declaration gives it, in
// either order.
export namespace a {
  interface Function<T> {
    unary(): Function<() => ReturnType<T>>;
  }
  interface Function<T extends (...args: any) => any> {
    throttle(): Function<T>;
  }
}
export namespace b {
  interface Function<T extends (...args: any) => any> {
    throttle(): Function<T>;
  }
  interface Function<T> {
    unary(): Function<() => ReturnType<T>>;
  }
}
