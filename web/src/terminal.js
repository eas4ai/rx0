// web/src/terminal.js
// Bottom-drawer terminal: an xterm.js frontend (global `Terminal` from
// the vendored UMD build) speaking to one PTY-backed shell over
// /api/terminal. Single session: closing the drawer detaches, the
// shell keeps running until the page or server goes away.
import { $, S } from './state.js';
import { showToast } from './ui.js';

const TKEY = 'rx0.drawer';
const MIN_H = 120, MAX_H = 600;

let term = null;
let ws = null;
let connectTimer = 0;

export const isTerminalOpen = () =>
  !document.body.classList.contains('drawer-hidden');

function enabled() {
  return S.settings?.['terminal.enabled'] !== false;
}

function termTheme() {
  const g = n => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
  return {
    background: g('--bg') || '#1e1e1e',
    foreground: g('--fg') || '#cccccc',
    cursor: g('--fg') || '#cccccc',
    selectionBackground: 'rgba(122,162,247,0.35)',
  };
}

function dims() {
  const fs = parseFloat(S.settings?.['editor.fontSize']) || 13.5;
  const el = $('#term-body');
  return {
    cols: Math.min(500, Math.max(20, Math.floor(el.clientWidth / (fs * 0.6)) || 80)),
    rows: Math.min(200, Math.max(5, Math.floor(el.clientHeight / (fs * 1.4)) || 24)),
  };
}

function fit() {
  if (!term) return;
  const { cols, rows } = dims();
  term.resize(cols, rows);
  if (ws?.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({ resize: [rows, cols] }));
  }
}

function connect() {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  const { cols, rows } = dims();
  ws = new WebSocket(`ws://${location.host}/api/terminal?rows=${rows}&cols=${cols}`);
  ws.binaryType = 'arraybuffer';
  ws.onmessage = e => {
    if (typeof e.data === 'string') term?.writeln(e.data);
    else term?.write(new Uint8Array(e.data));
  };
  ws.onclose = () => {
    ws = null;
    // The shell survives a dropped socket; reconnect on next open.
    if (isTerminalOpen()) {
      clearTimeout(connectTimer);
      connectTimer = setTimeout(() => isTerminalOpen() && connect(), 1000);
    }
  };
  ws.onerror = () => ws?.close();
}

function ensureTerm() {
  if (term) return true;
  if (typeof globalThis.Terminal === 'undefined') {
    showToast('!', 'Terminal frontend missing (xterm.js did not load)');
    return false;
  }
  const fs = parseFloat(S.settings?.['editor.fontSize']) || 13.5;
  term = new globalThis.Terminal({
    fontSize: fs,
    fontFamily: getComputedStyle(document.documentElement).getPropertyValue('--mono') || 'monospace',
    theme: termTheme(),
    scrollback: 5000,
  });
  term.open($('#term-body'));
  term.onData(d => {
    if (ws?.readyState === WebSocket.OPEN) ws.send(new TextEncoder().encode(d));
  });
  new MutationObserver(() => term?.setOption('theme', termTheme()))
    .observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
  fit();
  return true;
}

export function toggleTerminal(force) {
  const want = force !== undefined ? force : !isTerminalOpen();
  if (want && !enabled()) {
    showToast('i', 'Terminal is disabled (terminal.enabled)');
    return;
  }
  document.body.classList.toggle('drawer-hidden', !want);
  try { localStorage.setItem(TKEY, want ? '1' : '0'); } catch {}
  if (want) {
    if (!ensureTerm()) return;
    connect();
    requestAnimationFrame(() => { fit(); term?.focus(); });
  }
}

export function initTerminal() {
  document.body.classList.add('drawer-hidden');
  try {
    if (localStorage.getItem(TKEY) === '1') toggleTerminal(true);
  } catch {}
  $('#btn-term-close')?.addEventListener('click', () => toggleTerminal(false));

  (() => {
    const rz = $('#drawer-resizer');
    let dragging = false;
    rz?.addEventListener('mousedown', e => { dragging = true; e.preventDefault(); });
    addEventListener('mousemove', e => {
      if (!dragging) return;
      const h = Math.max(MIN_H, Math.min(MAX_H, innerHeight - e.clientY - 24));
      $('#drawer').style.height = h + 'px';
      fit();
    });
    addEventListener('mouseup', () => { dragging = false; });
  })();
  addEventListener('resize', () => isTerminalOpen() && fit());
}
