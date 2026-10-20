// Creates the "ts-r review" GitHub App from a manifest (one click in the browser), then writes
// its id and private key to the output directory for the box:
//   node box/create-github-app.mjs <out-dir> [org]
// Then install the app on the repository (the page links to it) and copy
// <out-dir>/github-app.pem to /srv/ts-r-review/secrets/ on the box, GITHUB_APP_ID to config.env.
import crypto from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import { execFile } from 'node:child_process';

const [outDir, org = 'Webb-Ventures'] = process.argv.slice(2);
if (!outDir) {
  console.error('usage: node box/create-github-app.mjs <out-dir> [org]');
  process.exit(2);
}
const PORT = 3456;
const state = crypto.randomBytes(16).toString('hex');
const manifest = {
  name: 'ts-r review',
  url: 'https://github.com/Webb-Ventures/tsc-rust',
  description: 'Codex code review for ts-r pull requests, run on the review box.',
  redirect_url: `http://127.0.0.1:${PORT}/callback`,
  public: false,
  hook_attributes: { url: 'https://github.com/Webb-Ventures/tsc-rust', active: false },
  default_events: [],
  default_permissions: {
    // Contents write only because GitHub requires it to resolve review threads.
    contents: 'write',
    pull_requests: 'write',
    issues: 'write',
    actions: 'read',
    metadata: 'read',
  },
};
const escape = (s) => s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;');

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  if (url.pathname === '/') {
    res.writeHead(200, { 'content-type': 'text/html' }).end(`<!doctype html>
<form method="post" action="https://github.com/organizations/${org}/settings/apps/new?state=${state}">
<p>Creates the <b>ts-r review</b> GitHub App in ${org}. You can rename it on the next page.</p>
<input type="hidden" name="manifest" value="${escape(JSON.stringify(manifest))}">
<button>Create GitHub App on GitHub</button></form>`);
    return;
  }
  if (url.pathname === '/callback' && url.searchParams.get('state') === state) {
    const response = await fetch(`https://api.github.com/app-manifests/${url.searchParams.get('code')}/conversions`, {
      method: 'POST', headers: { accept: 'application/vnd.github+json' },
    });
    const app = await response.json();
    if (!response.ok) {
      res.writeHead(500).end(`GitHub refused the conversion: ${JSON.stringify(app)}`);
      return;
    }
    fs.mkdirSync(outDir, { recursive: true, mode: 0o700 });
    fs.writeFileSync(`${outDir}/github-app.pem`, app.pem, { mode: 0o600 });
    fs.writeFileSync(`${outDir}/github-app.json`, JSON.stringify({ id: app.id, slug: app.slug, html_url: app.html_url }, null, 2));
    const install = `https://github.com/apps/${app.slug}/installations/new`;
    res.writeHead(200, { 'content-type': 'text/html' }).end(`<!doctype html><p>Created <b>${escape(app.name)}</b> (id ${app.id}).</p>
<p>Next: <a href="${install}">install it on Webb-Ventures/tsc-rust only</a>. You can close this tab after that.</p>`);
    console.log(JSON.stringify({ id: app.id, slug: app.slug, install }));
    setTimeout(() => process.exit(0), 500);
    return;
  }
  res.writeHead(404).end();
});
server.listen(PORT, '127.0.0.1', () => {
  const url = `http://127.0.0.1:${PORT}/`;
  console.error(`Open ${url} to create the app.`);
  execFile(process.platform === 'darwin' ? 'open' : 'xdg-open', [url], () => {});
});
