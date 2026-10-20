// Message event data passed on as a typed value.
type ServerEvent = { readonly kind: "tick"; readonly at: number } | { readonly kind: "stop" };
function show(ev: ServerEvent): void {
  console.log(ev.kind);
}
window.addEventListener("message", (e) => {
  show(e.data); // @expect TSR1401
});
export {};
