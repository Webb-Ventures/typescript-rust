// A class field copied to a local before the null check.
class Connection {
  socket: WebSocket | null = null;
  send(message: string): void {
    const socket = this.socket;
    if (socket !== null) socket.send(message);
  }
}
new Connection().send("x");
export {};
