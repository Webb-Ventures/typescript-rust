// Measurements cite the commit they were taken on. Those before the move to this repository
// were taken in the development repository, whose commits this one doesn't have (its history
// arrived as squashed sync commits), so a commit is linked only when the checkout the site is
// built from has it; otherwise its id is shown as plain text. pages.yml checks out the full
// history for this.
import { execFileSync } from 'node:child_process';
import path from 'node:path';

// Run from landing/ (npm scripts and `next build` do).
const ROOT = path.dirname(process.cwd());
const known = new Map();

/** Whether `sha` is a commit in this repository's history. */
export function published(sha) {
  if (!/^[0-9a-f]{7,40}$/.test(sha)) return false;
  let hit = known.get(sha);
  if (hit === undefined) {
    try {
      execFileSync('git', ['-C', ROOT, 'cat-file', '-e', `${sha}^{commit}`], { stdio: 'ignore' });
      hit = true;
    } catch {
      hit = false;
    }
    known.set(sha, hit);
  }
  return hit;
}
