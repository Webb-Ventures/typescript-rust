// Stored JSON read back without parsing.
function theme(): string {
  return JSON.parse(localStorage.getItem("prefs") ?? "{}").theme; // @expect TSR1401
}
export const t: string = theme();
