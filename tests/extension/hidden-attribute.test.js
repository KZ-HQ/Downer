"use strict";

/**
 * The `hidden` attribute is not a guarantee, it is a UA stylesheet declaration:
 * `[hidden] { display: none }` at the lowest possible precedence. An author rule
 * that sets `display` on the same element wins, and the element stays on screen
 * while the JavaScript that set `hidden` reads as correct.
 *
 * That is KEI-97: `.variants { display: flex }` kept the Quality picker visible
 * for playlists that offer no choice, even though `popup.js` had set
 * `row.variants.hidden = true`.
 *
 * These assertions are a **proxy** for the rendered result, not the result
 * itself. The jsdom harness in `helpers/extension-dom.js` cannot see this class
 * of bug: it does not load stylesheets, and its `getComputedStyle` reports
 * `display: none` for a `[hidden]` element whether or not an author rule
 * overrides it — so a rendering test there passes on the broken stylesheet.
 * Firefox is the only place the real behaviour shows, which is why the guard is
 * expressed against the stylesheet text instead.
 */

const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const assert = require("node:assert/strict");

const EXTENSION_DIR = path.join(__dirname, "..", "..", "extension");

/** Every stylesheet the extension ships. */
const STYLESHEETS = fs
  .readdirSync(EXTENSION_DIR)
  .filter((name) => name.endsWith(".css"))
  .sort();

function source(name) {
  return fs.readFileSync(path.join(EXTENSION_DIR, name), "utf8");
}

/** Strip comments so a rule quoted in prose is not mistaken for a declaration. */
function declarations(css) {
  return css.replace(/\/\*[\s\S]*?\*\//g, "");
}

test("the extension ships stylesheets for this to be about", () => {
  assert.ok(STYLESHEETS.length > 0);
});

for (const name of STYLESHEETS) {
  test(`${name} restores the hidden attribute over author display rules`, () => {
    const css = declarations(source(name));
    assert.match(
      css,
      /\[hidden\]\s*\{[^}]*display:\s*none\s*!important\s*;?[^}]*\}/,
      `${name} must carry [hidden] { display: none !important } — without it, ` +
        "any rule that sets display defeats the hidden attribute"
    );
  });

  test(`${name} declares no other important display, which would outrank the guard`, () => {
    const css = declarations(source(name));
    const important = [...css.matchAll(/([^{}]*)\{([^}]*)\}/g)]
      .filter(([, , body]) => /display:[^;]*!important/.test(body))
      .map(([, selector]) => selector.trim());
    assert.deepEqual(
      important,
      ["[hidden]"],
      `only the [hidden] guard in ${name} may declare display with !important; ` +
        "another one would win the cascade and hide nothing"
    );
  });
}
