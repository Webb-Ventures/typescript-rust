// A class's polymorphic `this` passed to a generic constructor or function infers
// through its constraint: `new Wrap(this, this)` inside `Box<T>` infers `T`.
interface Sink<T> { next: (value: T) => void; error: (err: any) => void }
class Source<T> {
  subscribe(o?: Partial<Sink<T>>): void {}
}
class Box<T> extends Source<T> {
  next(value: T) {}
  error(err: any) {}
  lift(): void {
    const w = new Wrap(this, this);
    const n: number = w;
    const v: string = unwrap(this);
  }
}
class Wrap<T> extends Box<T> {
  constructor(public sink?: Sink<T>, source?: Source<T>) {
    super();
  }
}
declare function unwrap<T>(s: Source<T>): T;
