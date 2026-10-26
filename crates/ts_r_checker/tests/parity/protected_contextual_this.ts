// A function expression whose contextual signature has `this: Foo` may read Foo's protected
// members (not its private ones).
class Foo {
  protected protec = "bar";
  private privat = "";
  copy!: string;
}
type BindingFunction = (this: Foo) => void;
export const bindCopy: BindingFunction = function () {
  this.copy = this.protec;
  void this.privat;
};
