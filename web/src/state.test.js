// web/src/state.test.js
// Focused unit tests for the shortcut-label helpers: the menu,
// footer buttons, and helpsheet all render through these.
import { describe, expect, test } from '@rstest/core';
import { keyCaps, keyLabel, withKeys } from './state.js';

describe('keyLabel (non-Mac platform)', () => {
  test.each([
    ['Mod+P', 'Ctrl+P'],
    ['Alt+Shift+H', 'Alt+Shift+H'],
    ['F12', 'F12'],
    ['`', '`'],
  ])('%s renders as %s', (combo, want) => {
    expect(keyLabel(combo)).toBe(want);
  });
});

describe('withKeys', () => {
  test('replaces {combo} spans in titles', () => {
    expect(withKeys('Go to File ({Mod+P})')).toBe('Go to File (Ctrl+P)');
  });

  test('leaves plain text alone', () => {
    expect(withKeys('Toggle terminal drawer')).toBe('Toggle terminal drawer');
  });
});

describe('keyCaps', () => {
  test('wraps each key in kbd', () => {
    expect(keyCaps('Alt+U')).toBe('<kbd>Alt</kbd><kbd>U</kbd>');
  });
});
