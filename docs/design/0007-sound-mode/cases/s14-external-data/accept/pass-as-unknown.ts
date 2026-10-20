// Passing an external value where unknown is expected makes no claim.
function log(value: unknown): void {
  console.log(value);
}
log(JSON.parse("[1, 2]"));
export {};
