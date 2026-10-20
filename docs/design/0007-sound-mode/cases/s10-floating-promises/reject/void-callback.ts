// An async callback where queueMicrotask expects a function returning void (VoidFunction).
async function save(x: string): Promise<void> {
  console.log(x);
}
queueMicrotask(async () => { await save("x"); }); // @expect TSR1003
export {};
