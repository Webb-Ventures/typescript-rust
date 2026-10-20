// A field asserted to be assigned somewhere.
class View {
  el!: HTMLElement; // @expect TSR0801
  attach(el: HTMLElement): void {
    this.el = el;
  }
}
new View().attach(document.body);
export {};
