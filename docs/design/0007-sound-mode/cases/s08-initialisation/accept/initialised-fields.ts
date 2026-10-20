// Fields set by initialisers or in the constructor before any read.
class Stats {
  items: string[] = [];
  size: number;
  constructor() {
    this.size = this.items.length;
  }
}
export const n: number = new Stats().size;
