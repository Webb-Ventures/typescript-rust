// A base constructor calls a method a subclass can override.
class Base {
  constructor() {
    this.setup(); // @expect TSR0803
  }
  setup(): void {
    console.log("base");
  }
}
new Base();
export {};
