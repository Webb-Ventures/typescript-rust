// Asserting a nullable field inside a method.
class Client {
  socket: WebSocket | null = null;
  send(message: string): void {
    this.socket!.send(message); // @expect TSR0301
  }
}
new Client().send("x");
export {};
