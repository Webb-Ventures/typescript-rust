// void hides the promise but not its rejection.
async function save(x: string): Promise<void> {
  console.log(x);
}
void save("a"); // @expect TSR1002
export {};
