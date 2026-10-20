// Parameter properties are assigned before the constructor body runs.
class Queue<T> {
  constructor(private readonly items: readonly T[]) {}
  get length(): number {
    return this.items.length;
  }
}
export const n: number = new Queue([1, 2]).length;
