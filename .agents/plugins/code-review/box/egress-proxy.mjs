// Egress for the Codex containers: an HTTPS CONNECT proxy that only reaches allowlisted hosts.
// Each profile runs as its own proxy on its own internal Docker network with no other route
// out, so even a prompt-injected model cannot send the code or its login anywhere else.
//   review  the two hosts Codex needs on a ChatGPT login (the Codex backend on chatgpt.com,
//           token refresh on auth.openai.com): the Codex containers, which hold that login.
//           Not api.openai.com, which would take an upload under anyone's API key.
//   deps    the package registries (crates.io, npm, JSR's npm mirror, Yarn, GitHub for git
//           dependencies), for the steps that install a fix's or a bench's dependencies. That container holds no login and no token: a
//           CONNECT tunnel to these hosts would also carry authenticated uploads, so it must
//           never be given to a container with anything secret in it.
import http from 'node:http';
import net from 'node:net';

const OPENAI = [/^chatgpt\.com$/, /^auth\.openai\.com$/];
const PROFILES = {
  review: OPENAI,
  deps: [
    /^crates\.io$/, /^index\.crates\.io$/, /^static\.crates\.io$/,
    /^registry\.npmjs\.org$/, /^npm\.jsr\.io$/, /^registry\.yarnpkg\.com$/, /^repo\.yarnpkg\.com$/,
    /^github\.com$/, /^codeload\.github\.com$/, /^objects\.githubusercontent\.com$/, /^release-assets\.githubusercontent\.com$/,
  ],
};
const profile = process.env.EGRESS_PROFILE ?? 'review';
const ALLOW = PROFILES[profile];
if (!ALLOW) throw new Error(`unknown EGRESS_PROFILE ${profile}`);

const server = http.createServer((req, res) => {
  res.writeHead(403).end('CONNECT only\n');
});
server.on('connect', (req, client, head) => {
  const [host, port] = req.url.split(':');
  if (port !== '443' || !ALLOW.some((re) => re.test(host))) {
    console.log(`deny ${req.url}`);
    client.end('HTTP/1.1 403 Forbidden\r\n\r\n');
    return;
  }
  console.log(`allow ${host}`);
  const upstream = net.connect(443, host, () => {
    client.write('HTTP/1.1 200 Connection Established\r\n\r\n');
    upstream.write(head);
    upstream.pipe(client);
    client.pipe(upstream);
  });
  upstream.on('error', () => client.destroy());
  client.on('error', () => upstream.destroy());
});
server.listen(3128, () => console.log(`egress proxy (${profile}) on :3128`));
