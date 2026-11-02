import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createComposer } from './hangul.js';

// Feed a sequence of jamo through a fresh composer and return the full text
// (committed portions + the final flush).
function type(jamos) {
  const c = createComposer();
  let out = '';
  for (const j of jamos) out += c.input(j).commit;
  out += c.flush();
  return out;
}

test('안녕하세요', () => {
  assert.equal(
    type(['ㅇ', 'ㅏ', 'ㄴ', 'ㄴ', 'ㅕ', 'ㅇ', 'ㅎ', 'ㅏ', 'ㅅ', 'ㅔ', 'ㅇ', 'ㅛ']),
    '안녕하세요',
  );
});

test('한글', () => {
  assert.equal(type(['ㅎ', 'ㅏ', 'ㄴ', 'ㄱ', 'ㅡ', 'ㄹ']), '한글');
});

test('닭가 — compound final does not migrate when a consonant follows (ㄷㅏㄹㄱㄱㅏ)', () => {
  assert.equal(type(['ㄷ', 'ㅏ', 'ㄹ', 'ㄱ', 'ㄱ', 'ㅏ']), '닭가');
});

test('달가 — compound final splits and migrates when a vowel follows (ㄷㅏㄹㄱㅏ)', () => {
  assert.equal(type(['ㄷ', 'ㅏ', 'ㄹ', 'ㄱ', 'ㅏ']), '달가');
});

test('없어 — compound final ㅄ then migration (ㅇㅓㅂㅅㅇㅓ)', () => {
  assert.equal(type(['ㅇ', 'ㅓ', 'ㅂ', 'ㅅ', 'ㅇ', 'ㅓ']), '없어');
});

test('괜찮아 — compound vowel + compound final + migration', () => {
  assert.equal(
    type(['ㄱ', 'ㅗ', 'ㅐ', 'ㄴ', 'ㅊ', 'ㅏ', 'ㄴ', 'ㅎ', 'ㅇ', 'ㅏ']),
    '괜찮아',
  );
});

test('의사 — ㅢ compound vowel then final migration', () => {
  assert.equal(type(['ㅇ', 'ㅡ', 'ㅣ', 'ㅅ', 'ㅏ']), '의사');
});

test('뷁 — compound vowel ㅞ + compound final ㄺ', () => {
  assert.equal(type(['ㅂ', 'ㅜ', 'ㅔ', 'ㄹ', 'ㄱ']), '뷁');
});

test('doubled consonants: 까 / 있 / 깎', () => {
  assert.equal(type(['ㄲ', 'ㅏ']), '까');
  assert.equal(type(['ㅇ', 'ㅣ', 'ㅆ']), '있');
  assert.equal(type(['ㄲ', 'ㅏ', 'ㄲ']), '깎');
});

test('ㄸㅃㅉ cannot be finals — start a new syllable', () => {
  // ㅏ + ㄸ + ㅏ → 아따 (ㄸ can begin a syllable but never end one)
  assert.equal(type(['ㅇ', 'ㅏ', 'ㄸ', 'ㅏ']), '아따');
});

test('lone jamo survive as compatibility characters', () => {
  assert.equal(type(['ㅎ', 'ㅎ']), 'ㅎㅎ');
  assert.equal(type(['ㅏ']), 'ㅏ');
  assert.equal(type(['ㅗ', 'ㅏ']), 'ㅘ'); // lone compound vowel
});

test('backspace decomposes step-wise: 한 → 하 → ㅎ → empty', () => {
  const c = createComposer();
  c.input('ㅎ');
  c.input('ㅏ');
  c.input('ㄴ'); // 한
  assert.equal(c.backspace().composing, '하');
  assert.equal(c.backspace().composing, 'ㅎ');
  assert.equal(c.backspace().composing, '');
  assert.equal(c.backspace().handled, false); // empty → pass through
});

test('backspace peels a compound final then the vowel: 닭 → 달 → 다 → ㄷ', () => {
  const c = createComposer();
  ['ㄷ', 'ㅏ', 'ㄹ', 'ㄱ'].forEach((j) => c.input(j)); // 닭
  assert.equal(c.backspace().composing, '달');
  assert.equal(c.backspace().composing, '다');
  assert.equal(c.backspace().composing, 'ㄷ');
  assert.equal(c.backspace().composing, '');
});

test('backspace peels a compound vowel: 의 → 으 → ㅇ', () => {
  const c = createComposer();
  ['ㅇ', 'ㅡ', 'ㅣ'].forEach((j) => c.input(j)); // 의
  assert.equal(c.backspace().composing, '으');
  assert.equal(c.backspace().composing, 'ㅇ');
});

test('flush mid-syllable commits the live char and resets', () => {
  const c = createComposer();
  c.input('ㅎ');
  c.input('ㅏ'); // 하 in progress
  assert.equal(c.flush(), '하');
  // composer is reset — the next jamo starts clean
  assert.equal(c.input('ㄱ').composing, 'ㄱ');
});
