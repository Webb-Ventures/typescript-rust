// A stand-in for tests/utils/pty-shell (it needs the native `pty-spawn` package). Tests that open
// a pty shell are skipped by spec-runner.mjs before they run.

export const isWindows = process.platform === 'win32';

export const ptyShell = () => {
  throw Object.assign(new Error('pty shell'), { oracleSkip: 'interactive pty shell' });
};
