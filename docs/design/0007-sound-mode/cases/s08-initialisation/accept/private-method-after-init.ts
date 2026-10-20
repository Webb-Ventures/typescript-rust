// A #private method can't be overridden, so it may run once every field is set.
class Counter {
  #count: number;
  #label: string;
  constructor(label: string) {
    this.#count = 0;
    this.#label = label;
    this.#log();
  }
  #log(): void {
    console.log(this.#label, this.#count);
  }
}
new Counter("a");
export {};
