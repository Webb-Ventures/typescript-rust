// Copying the property to a const before the await keeps the narrowing.
interface Conn {
  socket: WebSocket | null;
}
async function send(conn: Conn, flush: () => Promise<void>, m: string): Promise<void> {
  const socket = conn.socket;
  if (socket !== null) {
    await flush();
    socket.send(m);
  }
}
export { send };
