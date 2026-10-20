// Work that needs a complete object happens after construction.
class Widget {
  mounted = false;
  mount(): void {
    this.mounted = true;
  }
  static create(): Widget {
    const w = new Widget();
    w.mount();
    return w;
  }
}
export const mounted: boolean = Widget.create().mounted;
