// A method reads a field before the constructor sets it.
class List {
  items: string[];
  constructor() {
    this.render(); // @expect TSR0802
    this.items = [];
  }
  render(): void {
    console.log(this.items.length);
  }
}
new List();
export {};
