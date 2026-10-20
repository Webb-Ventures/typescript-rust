// An arrow function that mentions this is handed out before count is set.
class Clicker {
  count: number;
  constructor(target: EventTarget) {
    target.addEventListener("click", () => this.count++); // @expect TSR0802
    this.count = 0;
  }
}
new Clicker(document.body);
export {};
