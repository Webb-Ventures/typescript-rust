// box-run (installed in the image): runs a build, test or project command for Codex in the fix
// job's runner container (job.mjs runner), which has the toolchain and the dependencies but no
// network and no Codex login. The Codex container has neither the toolchain nor the
// dependencies, so code from the PR (build scripts, tests, package scripts) only ever runs there.
//   box-run cargo test -p ts-r-parser
//   box-run 'cd packages/x && npm run build'
// Prints the command's output and exits with its status. Requests go through RUN_DIR, a
// directory both containers mount, and run one at a time in the order they were made.
import crypto from 'node:crypto';
import fs from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';

const dir = process.env.RUN_DIR;
const args = process.argv.slice(2);
if (!dir || args.length === 0) {
  console.error('usage: box-run <command> [args...]   (in a fix job only)');
  process.exit(2);
}
// One argument is a shell script; several are a command line, quoted as given.
const quote = (a) => (/^[\w@%+=:,./-]+$/.test(a) ? a : `'${a.replaceAll("'", `'\\''`)}'`);
const script = args.length === 1 ? args[0] : args.map(quote).join(' ');
const id = `${Date.now()}-${crypto.randomUUID()}`;
fs.writeFileSync(`${dir}/${id}.new`, JSON.stringify({ script, cwd: process.cwd() }));
fs.renameSync(`${dir}/${id}.new`, `${dir}/${id}.req`);
const alive = () => {
  try {
    return Date.now() - fs.statSync(`${dir}/alive`).mtimeMs < 30_000;
  } catch {
    return false;
  }
};
while (!fs.existsSync(`${dir}/${id}.done`)) {
  if (!alive()) {
    fs.rmSync(`${dir}/${id}.req`, { force: true });
    console.error('box-run: the runner is not running');
    process.exit(125);
  }
  await sleep(250);
}

const { status } = JSON.parse(fs.readFileSync(`${dir}/${id}.done`, 'utf8'));
const out = fs.readFileSync(`${dir}/${id}.out`);
const LIMIT = 2 << 20;
process.stdout.write(out.length > LIMIT ? Buffer.concat([Buffer.from('[earlier output cut]\n'), out.subarray(-LIMIT)]) : out);
for (const file of [`${id}.out`, `${id}.done`]) fs.rmSync(`${dir}/${file}`, { force: true });
process.exit(status);
