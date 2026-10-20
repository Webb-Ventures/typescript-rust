// Starting async work on this before data is set.
class Loader {
  data: string[];
  constructor() {
    void this.load().catch(() => undefined); // @expect TSR0802
    this.data = [];
  }
  async load(): Promise<void> {
    this.data.push("x");
  }
}
new Loader();
export {};
