// A type parameter constrained to a union holding another type parameter compares with it;
// it is not assignable to it.
class C<T> {
  constructor(readonly x: T) {}
  good<U extends T>(y: U) {
    if (y === this.x) {}
  }
  bad<U extends T | string>(y: U) {
    if (y === this.x) {}
    const t: T = y;
  }
  inter<U extends T & { a: 1 }>(y: U) {
    const t: T = y;
  }
}
