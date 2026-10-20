// Assigning undefined to an optional field.
class Draft {
  title?: string;
  clear(): void {
    this.title = undefined; // @expect TSR0402
  }
}
new Draft().clear();
export {};
