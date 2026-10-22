// A class type parameter named inside a static member is TS2302 and the error type, so
// nothing else is reported for it (no `null` to `T`, no `T` to `string`).
class List<T> {
  constructor(public isHead: boolean, public data: T) {}
  static make(): List<T> {
    const entry: List<T> = new List<T>(true, null);
    return entry;
  }
  static take(x: T): string {
    return x;
  }
}
