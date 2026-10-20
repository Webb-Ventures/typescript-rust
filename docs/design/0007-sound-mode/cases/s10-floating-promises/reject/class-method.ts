// A method that starts an async flush and forgets it.
class Store {
  async flush(): Promise<void> {
    console.log("flush");
  }
  close(): void {
    this.flush(); // @expect TSR1001
  }
}
new Store().close();
export {};
