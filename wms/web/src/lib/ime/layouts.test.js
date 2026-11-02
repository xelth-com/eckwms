import { test } from 'node:test';
import assert from 'node:assert/strict';
import { LAYOUTS, imeLangFor, resolveKey, isNonLatin } from './layouts.js';

// Type a phrase by physical key codes and concatenate the resolved characters.
function typeRu(codes, mods = {}) {
  return codes.map((code) => resolveKey('ru', code, mods) ?? '').join('');
}

test('imeLangFor is registry-driven', () => {
  assert.equal(imeLangFor('ru'), 'ru');
  assert.equal(imeLangFor('ko'), 'ko');
  assert.equal(imeLangFor('en'), null);
  assert.equal(imeLangFor('de'), null);
  assert.equal(imeLangFor(null), null);
  assert.equal(imeLangFor('xx'), null);
});

test('ru: "привет мир" by physical key position', () => {
  // п р и в е т _ м и р
  const codes = ['KeyG', 'KeyH', 'KeyB', 'KeyD', 'KeyT', 'KeyN'];
  assert.equal(typeRu(codes), 'привет');
  assert.equal(typeRu(['KeyV', 'KeyB', 'KeyH']), 'мир');
});

test('ru: full letter rows map to ЙЦУКЕН by position', () => {
  const top = ['KeyQ', 'KeyW', 'KeyE', 'KeyR', 'KeyT', 'KeyY', 'KeyU', 'KeyI', 'KeyO', 'KeyP', 'BracketLeft', 'BracketRight'];
  assert.equal(typeRu(top), 'йцукенгшщзхъ');
  const home = ['KeyA', 'KeyS', 'KeyD', 'KeyF', 'KeyG', 'KeyH', 'KeyJ', 'KeyK', 'KeyL', 'Semicolon', 'Quote'];
  assert.equal(typeRu(home), 'фывапролджэ');
  const bottom = ['KeyZ', 'KeyX', 'KeyC', 'KeyV', 'KeyB', 'KeyN', 'KeyM'];
  assert.equal(typeRu(bottom), 'ячсмить');
});

test('ru: ё and its symbol keys', () => {
  assert.equal(resolveKey('ru', 'Backquote', {}), 'ё');
  assert.equal(resolveKey('ru', 'Backquote', { shift: true }), 'Ё');
  assert.equal(resolveKey('ru', 'Comma', {}), 'б');
  assert.equal(resolveKey('ru', 'Period', {}), 'ю');
  assert.equal(resolveKey('ru', 'Slash', {}), '.');
  assert.equal(resolveKey('ru', 'Slash', { shift: true }), ',');
  assert.equal(resolveKey('ru', 'Backslash', {}), '\\');
  assert.equal(resolveKey('ru', 'Backslash', { shift: true }), '/');
});

test('ru: shifted digit row is ! " № ; % : ? * ( )', () => {
  const digits = ['Digit1', 'Digit2', 'Digit3', 'Digit4', 'Digit5', 'Digit6', 'Digit7', 'Digit8', 'Digit9', 'Digit0'];
  assert.equal(typeRu(digits), '1234567890');
  assert.equal(typeRu(digits, { shift: true }), '!"№;%:?*()');
});

test('ru: CapsLock XOR Shift truth table for a letter key (KeyF → а/А)', () => {
  assert.equal(resolveKey('ru', 'KeyF', { shift: false, caps: false }), 'а');
  assert.equal(resolveKey('ru', 'KeyF', { shift: true, caps: false }), 'А');
  assert.equal(resolveKey('ru', 'KeyF', { shift: false, caps: true }), 'А');
  assert.equal(resolveKey('ru', 'KeyF', { shift: true, caps: true }), 'а');
});

test('ru: CapsLock does NOT affect symbol/digit keys', () => {
  assert.equal(resolveKey('ru', 'Digit1', { caps: true }), '1');
  assert.equal(resolveKey('ru', 'Digit1', { shift: true, caps: true }), '!');
});

test('ko: 2-beolsik positions emit compatibility jamo', () => {
  assert.equal(resolveKey('ko', 'KeyR', {}), 'ㄱ');
  assert.equal(resolveKey('ko', 'KeyR', { shift: true }), 'ㄲ');
  assert.equal(resolveKey('ko', 'KeyK', {}), 'ㅏ');
  assert.equal(resolveKey('ko', 'KeyD', {}), 'ㅇ');
  assert.equal(resolveKey('ko', 'KeyO', {}), 'ㅐ');
  assert.equal(resolveKey('ko', 'KeyO', { shift: true }), 'ㅒ');
});

test('ko: CapsLock never affects jamo (they have no case)', () => {
  assert.equal(resolveKey('ko', 'KeyR', { caps: true }), 'ㄱ');
  assert.equal(resolveKey('ko', 'KeyR', { shift: true, caps: true }), 'ㄲ');
});

test('unmapped codes and unknown langs return null', () => {
  assert.equal(resolveKey('ru', 'Space', {}), null);
  assert.equal(resolveKey('ko', 'Digit1', {}), null); // ko digits pass through
  assert.equal(resolveKey('en', 'KeyA', {}), null);
});

test('isNonLatin: Latin scripts (incl. German umlauts, ß, §) are NOT non-Latin', () => {
  for (const ch of ['a', 'Z', 'ö', 'ä', 'ü', 'Ö', 'ß', '§', '°', 'é', 'ñ', 'ł', '1', '.', ' ']) {
    assert.equal(isNonLatin(ch), false, `${ch} should be treated as Latin`);
  }
});

test('isNonLatin: Cyrillic and Hangul ARE non-Latin', () => {
  for (const ch of ['й', 'ж', 'э', 'х', 'Ё', 'ㄱ', 'ㅏ', '한', '한'.normalize()]) {
    assert.equal(isNonLatin(ch), true, `${ch} should be non-Latin`);
  }
  assert.equal(isNonLatin(''), false);
  assert.equal(isNonLatin(null), false);
});

test('isNonLatin: boundary is U+024F (Latin Extended-B)', () => {
  assert.equal(isNonLatin(String.fromCodePoint(0x024f)), false); // last Latin
  assert.equal(isNonLatin(String.fromCodePoint(0x0250)), true); // first beyond
});

test('LAYOUTS registry exposes ru and ko', () => {
  assert.deepEqual(Object.keys(LAYOUTS).sort(), ['ko', 'ru']);
});
