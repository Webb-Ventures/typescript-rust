// ts-r-run's half of the signal relay (the other half is src/unix.rs). ts-r-run stays Node's
// parent and, on SIGINT, SIGTERM or SIGHUP, waits for this process to say it got the signal
// before it sends it on or, failing an answer, kills this process.
//
// A listener first in line on each of those signals sends SIGURG to ts-r-run ("got it"),
// then ends the process as Node's default action would when the program has no listener of
// its own. The listener is hidden from `process.listeners()` and `process.listenerCount()`
// (it stays in `process.rawListeners()`), so a program that checks for listeners sees its own.
import { constants } from 'node:os';

const parent = Number(process.env.TS_R_RUN_RELAY);
delete process.env.TS_R_RUN_RELAY;

if (parent > 0) {
  const hidden = new Map();
  for (const name of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    const onSignal = () => {
      if (process.ppid === parent) {
        try {
          process.kill(parent, 'SIGURG');
        } catch {}
      }
      if (process.listenerCount(name) === 0) process.exit(128 + constants.signals[name]);
    };
    process.prependListener(name, onSignal);
    hidden.set(name, onSignal);
  }
  const { listenerCount, listeners } = process;
  process.listenerCount = function (name, listener) {
    const n = Reflect.apply(listenerCount, this, arguments);
    const own = hidden.get(name);
    // Counting one given listener only hides ours when ours is the one asked about.
    if (own === undefined || (arguments.length > 1 && listener != null && listener !== own)) return n;
    return n - Reflect.apply(listeners, this, [name]).filter((l) => l === own).length;
  };
  process.listeners = function (name) {
    const all = Reflect.apply(listeners, this, arguments);
    const own = hidden.get(name);
    return own === undefined ? all : all.filter((l) => l !== own);
  };
}
