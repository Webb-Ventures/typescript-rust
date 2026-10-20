// then without a rejection handler is still floating.
function log(r: Response): void {
  console.log(r.status);
}
fetch("/ping").then(log); // @expect TSR1001
export {};
