// HTMLElement to a more specific element type.
function valueOf(el: HTMLElement): string {
  const input = el as HTMLInputElement; // @expect TSR0201
  return input.value;
}
export const v: string = valueOf(document.body);
