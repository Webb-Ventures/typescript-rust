// The sound-mode escape hatch replaces @ts-ignore, with a reason.
function title(): string {
  // @ts-r-unsafe S-03: the page template always renders #title.
  return document.getElementById("title")!.textContent ?? "";
}
export const t: string = title();
