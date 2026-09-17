// web/src/history.js
import { S } from './state.js';
import { openFile } from './tabs.js';

export function pushHistory(path, line) {
  const top = S.hist[S.histIdx];
  if (top && top.path === path && Math.abs(top.line - line) < 2) return;
  S.hist = S.hist.slice(0, S.histIdx + 1);
  S.hist.push({ path, line });
  if (S.hist.length > 120) S.hist.shift();
  S.histIdx = S.hist.length - 1;
}

export function go(delta) {
  const i = S.histIdx + delta;
  if (i < 0 || i >= S.hist.length) return;
  S.histIdx = i;
  const h = S.hist[i];
  openFile(h.path, { line: h.line, push: false });
}
