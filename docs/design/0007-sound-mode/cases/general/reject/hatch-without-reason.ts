// A hatch must give a reason; this one suppresses nothing.
// @ts-r-unsafe // @expect TSR0001
const el = document.getElementById("app")!; // @expect TSR0301
export const tag: string = el.tagName;
