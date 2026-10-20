// An async method awaited inside another async method.
class Store {
  async flush(): Promise<void> {
    console.log("flush");
  }
  async close(): Promise<void> {
    await this.flush();
  }
}
await new Store().close();
export {};
