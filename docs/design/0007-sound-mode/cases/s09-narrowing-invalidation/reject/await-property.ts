// Other code can run during await and clear the socket.
interface Conn {
  socket: WebSocket | null;
}
async function send(conn: Conn, flush: () => Promise<void>, m: string): Promise<void> {
  if (conn.socket !== null) {
    await flush();
    conn.socket.send(m); // @expect TSR0901
  }
}
export { send };
