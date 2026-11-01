#!/usr/bin/env node
// Tests the ICE console's terminal (the `Term` class in src/ice/ui.html):
//   node scripts/test-ui-term.js
"use strict";
const fs = require("fs");
const path = require("path");
const html = fs.readFileSync(path.join(__dirname, "../src/ice/ui.html"), "utf8");
const src = html.slice(html.indexOf("// ---- Term begin"), html.indexOf("// ---- Term end"));
const Term = new Function(src + "\nreturn Term;")();
const assert = require("assert");
let n = 0;
function test(name, f) {
  try {
    f();
    n++;
  } catch (e) {
    console.error(`FAIL ${name}\n${e.message}`);
    process.exit(1);
  }
}
const term = (mode = "ansi", cols = 20, rows = 5) => {
  const t = new Term(cols, rows, 100);
  t.mode = mode;
  return t;
};
const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;");

test("lines, CR LF and wrapping", () => {
  const t = term();
  t.write("hello\r\nworld\r\n12345678901234567890X");
  assert.strictEqual(t.text(), "hello\nworld\n12345678901234567890\nX");
  t.write("\r\rA\bB"); // CR CR then backspace overwrite
  assert.strictEqual(t.text().split("\n")[3], "B");
});

test("scrolling keeps a scrollback", () => {
  const t = term();
  for (let i = 0; i < 8; i++) t.write(`line${i}\r\n`);
  assert.strictEqual(t.history.length, 4);
  assert.strictEqual(t.text(), "line4\nline5\nline6\nline7");
  assert.ok(t.html(esc).startsWith("line0\nline1"));
});

test("VT52: direct cursor address, erase, moves", () => {
  const t = term("vt52");
  t.write("abcdef\r\nghijkl");
  t.write("\x1bY" + String.fromCharCode(32 + 0, 32 + 2) + "X"); // row 0 col 2
  assert.strictEqual(t.text().split("\n")[0], "abXdef");
  t.write("\x1bK"); // erase to end of line
  assert.strictEqual(t.text().split("\n")[0], "abX");
  t.write("\x1bB\x1bD\x1bD*"); // from column 3: down, left, left
  assert.strictEqual(t.text().split("\n")[1], "g*ijkl");
  t.write("\x1bH\x1bJ"); // home, erase to end of screen
  assert.strictEqual(t.text(), "");
  t.write("\x1bY\x24\x20bottom\x1bA\x1bIup"); // row 4, then up and reverse index
  assert.strictEqual(t.y, 2);
});

test("ANSI: CUP, ED, EL, relative moves", () => {
  const t = term();
  t.write("\x1b[2J\x1b[3;5Hx\x1b[1;1Hy\x1b[2C\x1b[1Bz");
  assert.strictEqual(t.text(), "y\n   z\n    x");
  t.write("\x1b[3;1H\x1b[K");
  assert.strictEqual(t.text(), "y\n   z");
  t.write("\x1b[1;1H\x1b[J");
  assert.strictEqual(t.text(), "");
});

test("ANSI: SGR colours, bright colours, reverse", () => {
  const t = term();
  t.write("\x1b[94mMulticomp09\x1b[0m \x1b[1;31mE\x1b[7mR\x1b[m.");
  const h = t.html(esc);
  assert.ok(h.includes('<span class="f12">Multicomp09</span>'), h);
  assert.ok(h.includes('<span class="f1 tb">E</span>'), h);
  assert.ok(h.includes('<span class="f17 b1 tb">R</span>'), h);
  assert.ok(h.includes("R</span>.<span class=\"cursor\">"), h);
});

test("ANSI: scroll region, insert and delete lines", () => {
  const t = term();
  t.write("a\r\nb\r\nc\r\nd\r\ne");
  t.write("\x1b[2;4r\x1b[4;1H\nX"); // region rows 2-4; LF at its bottom scrolls it
  assert.strictEqual(t.text(), "a\nc\nd\nX\ne");
  assert.strictEqual(t.history.length, 0, "a scroll region does not feed the scrollback");
  t.write("\x1b[r\x1b[2;1H\x1b[L"); // full screen again; insert a line at row 2
  assert.strictEqual(t.text(), "a\n\nc\nd\nX");
  t.write("\x1b[M\x1b[M");
  assert.strictEqual(t.text(), "a\nd\nX");
});

test("ANSI: insert, delete and erase characters", () => {
  const t = term();
  t.write("abcdef\r\x1b[2C\x1b[2@");
  assert.strictEqual(t.text(), "ab  cdef");
  t.write("\x1b[3P");
  assert.strictEqual(t.text(), "abdef");
  t.write("\x1b[2X");
  assert.strictEqual(t.text(), "ab  f");
});

test("cursor report, save and restore, alternate screen", () => {
  const t = term();
  let got = "";
  t.reply = (s) => (got += s);
  t.write("\x1b[2;3H\x1b[6n");
  assert.strictEqual(got, "\x1b[2;3R");
  t.write("\x1b7\x1b[5;5H\x1b8Q");
  assert.strictEqual(t.text().split("\n")[1], "  Q");
  t.write("\x1b[?1049h");
  assert.strictEqual(t.text(), "");
  t.write("\x1b[?25l");
  assert.ok(!t.html(esc).includes("cursor"));
});

test("keys follow the mode", () => {
  const a = term("ansi"), v = term("vt52");
  assert.strictEqual(a.key({ key: "ArrowUp" }), "\x1b[A");
  assert.strictEqual(v.key({ key: "ArrowUp" }), "\x1bA");
  assert.strictEqual(a.key({ key: "PageDown" }), "\x1b[6~");
  assert.strictEqual(v.key({ key: "Delete" }), "\x7f");
  assert.strictEqual(a.key({ key: "a" }), null);
});

test("OSC titles and character set selections are skipped", () => {
  const t = term();
  t.write("\x1b]0;title\x07\x1b(Bok");
  assert.strictEqual(t.text(), "ok");
});

test("resize keeps the content and the cursor line", () => {
  const t = term("ansi", 20, 6);
  t.write("one\r\ntwo\r\nthree\r\nfour");
  // Fewer rows: the blank rows below the cursor go first.
  t.resize(20, 4);
  assert.strictEqual(t.text(), "one\ntwo\nthree\nfour");
  assert.strictEqual(t.history.length, 0);
  // Fewer again: the top row goes to the scrollback.
  t.resize(20, 3);
  assert.strictEqual(t.text(), "two\nthree\nfour");
  assert.deepStrictEqual([t.y, t.history.length], [2, 1]);
  // More rows: back from the scrollback.
  t.resize(20, 5);
  assert.strictEqual(t.text(), "one\ntwo\nthree\nfour");
  assert.strictEqual(t.y, 3);
  // Narrower: lines are cut; the cursor stays inside.
  t.write("-and-more");
  t.resize(10, 5);
  assert.strictEqual(t.text(), "one\ntwo\nthree\nfour-and-m");
  assert.ok(t.x <= 9);
  // Writing after a resize uses the new size (wraps at 10).
  t.write("\r\n123456789012");
  assert.strictEqual(t.text().split("\n").slice(-2).join("|"), "1234567890|12");
});

console.log(`ok ${n} tests`);
