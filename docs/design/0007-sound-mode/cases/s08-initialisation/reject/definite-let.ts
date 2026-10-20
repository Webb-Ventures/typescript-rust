// A local asserted to be assigned before use.
let count!: number; // @expect TSR0801
function init(): void {
  count = 1;
}
init();
export const c: number = count;
