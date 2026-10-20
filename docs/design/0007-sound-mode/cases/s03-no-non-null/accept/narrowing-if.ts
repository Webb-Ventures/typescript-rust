// A null check instead of !.
function setTitle(text: string): void {
  const el = document.getElementById("title");
  if (el !== null) el.textContent = text;
}
setTitle("hi");
export {};
