/**
 * Static server for the end-to-end fixtures.
 *
 * The pages under `tests/fixtures/pages/` are shared with `src/scraper.rs` and
 * the jsdom tests, so the end-to-end run scans exactly the same HTML the other
 * two layers do. `tests/fixtures/protected_site.py` stays the tool for the
 * cookie-gated scenarios; this server only needs to put those files on a real
 * origin so Firefox will inject the content script into them.
 */

import fs from "node:fs";
import http from "node:http";
import { once } from "node:events";
import path from "node:path";

const PAGES_DIR = path.join(import.meta.dirname, "..", "fixtures", "pages");

const CONTENT_TYPES = {
  ".html": "text/html; charset=utf-8",
  ".m3u8": "application/vnd.apple.mpegurl",
  ".mp4": "video/mp4"
};

/**
 * Starts the server on an ephemeral port and returns it with its base URL.
 *
 * `/pages/<name>.html` serves a fixture. Any other path answers with a small
 * body of the right content type, so a page's `<video src>` resolves instead of
 * leaving a network error in the console: the media bytes are irrelevant here,
 * only that the request happened and shows up in `performance.getEntries()`.
 */
export async function startFixtureServer() {
  const server = http.createServer((request, response) => {
    const url = new URL(request.url, "http://127.0.0.1");
    const extension = path.extname(url.pathname) || ".html";
    const type = CONTENT_TYPES[extension] || "application/octet-stream";

    if (url.pathname.startsWith("/pages/")) {
      const name = path.basename(url.pathname);
      const file = path.join(PAGES_DIR, name);
      if (path.dirname(file) !== PAGES_DIR || !fs.existsSync(file)) {
        response.writeHead(404, { "content-type": "text/plain" }).end("not found");
        return;
      }
      response.writeHead(200, { "content-type": type });
      fs.createReadStream(file).pipe(response);
      return;
    }

    response.writeHead(200, { "content-type": type }).end("stub");
  });

  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const { port } = server.address();

  return {
    server,
    origin: `http://127.0.0.1:${port}`,
    pageUrl: (name) => `http://127.0.0.1:${port}/pages/${name}.html`,
    async close() {
      server.closeAllConnections();
      server.close();
      await once(server, "close");
    }
  };
}
