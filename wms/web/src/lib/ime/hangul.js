// Pure, DOM-free 2-beolsik (두벌식) Hangul composition automaton.
//
// The keyboard layout (layouts.js `ko`) emits COMPATIBILITY jamo (U+3130 block),
// one per key. This module assembles them into syllables the way an OS IME
// would: initial (cho) + medial (jung) + optional final (jong), with compound
// vowels, compound finals, and final→initial migration when a vowel follows a
// completed final (닭 + ㅏ → 달가). It never touches the DOM — ime.js drives it.
//
//   const c = createComposer();
//   c.input('ㅎ') → { commit: '',  composing: 'ㅎ' }
//   c.input('ㅏ') → { commit: '',  composing: '하' }
//   c.input('ㄴ') → { commit: '',  composing: '한' }
//   c.flush()     → '한'
//
// `commit` is text that is now final (flushed to the left of the caret);
// `composing` is the single live character still open for editing.

// Compatibility-jamo tables (index = Unicode composition index).
const CHO = 'ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ'.split('');
const JUNG = 'ㅏㅐㅑㅒㅓㅔㅕㅖㅗㅘㅙㅚㅛㅜㅝㅞㅟㅠㅡㅢㅣ'.split('');
// index 0 = "no final"; the rest are the 27 possible finals.
const JONG = ['', ...'ㄱㄲㄳㄴㄵㄶㄷㄹㄺㄻㄼㄽㄾㄿㅀㅁㅂㅄㅅㅆㅇㅈㅊㅋㅌㅍㅎ'.split('')];

const CHO_MAP = {};
CHO.forEach((c, i) => (CHO_MAP[c] = i));
const JUNG_MAP = {};
JUNG.forEach((c, i) => (JUNG_MAP[c] = i));
const JONG_MAP = {};
// Only single, typeable finals get a direct entry (ㄸ/ㅃ/ㅉ are absent → cannot
// be finals). Compound finals are formed via FINAL_COMPOUND, not typed directly.
JONG.forEach((c, i) => {
  if (i > 0 && c.length === 1 && CHO_MAP[c] !== undefined) JONG_MAP[c] = i;
});

// jong index → cho index (for single-final migration).
const JONG_TO_CHO = {};
JONG.forEach((c, i) => {
  if (i > 0 && CHO_MAP[c] !== undefined) JONG_TO_CHO[i] = CHO_MAP[c];
});

// Compound finals: `${baseJongIndex}+${addedConsonantChar}` → compound jong idx.
const FINAL_COMPOUND = {};
// Reverse: compound jong idx → [baseJongIndex, secondConsonantChar].
const FINAL_DECOMPOSE = {};
function defFinal(baseChar, addChar, compoundChar) {
  const base = JONG_MAP[baseChar];
  const compound = JONG.indexOf(compoundChar);
  FINAL_COMPOUND[`${base}+${addChar}`] = compound;
  FINAL_DECOMPOSE[compound] = [base, addChar];
}
defFinal('ㄱ', 'ㅅ', 'ㄳ');
defFinal('ㄴ', 'ㅈ', 'ㄵ');
defFinal('ㄴ', 'ㅎ', 'ㄶ');
defFinal('ㄹ', 'ㄱ', 'ㄺ');
defFinal('ㄹ', 'ㅁ', 'ㄻ');
defFinal('ㄹ', 'ㅂ', 'ㄼ');
defFinal('ㄹ', 'ㅅ', 'ㄽ');
defFinal('ㄹ', 'ㅌ', 'ㄾ');
defFinal('ㄹ', 'ㅍ', 'ㄿ');
defFinal('ㄹ', 'ㅎ', 'ㅀ');
defFinal('ㅂ', 'ㅅ', 'ㅄ');

// Compound vowels: `${baseJungIndex}+${addedVowelChar}` → compound jung idx.
const VOWEL_COMPOUND = {};
// Reverse: compound jung idx → base jung idx.
const VOWEL_DECOMPOSE = {};
function defVowel(baseChar, addChar, compoundChar) {
  const base = JUNG_MAP[baseChar];
  const compound = JUNG_MAP[compoundChar];
  VOWEL_COMPOUND[`${base}+${addChar}`] = compound;
  VOWEL_DECOMPOSE[compound] = base;
}
defVowel('ㅗ', 'ㅏ', 'ㅘ');
defVowel('ㅗ', 'ㅐ', 'ㅙ');
defVowel('ㅗ', 'ㅣ', 'ㅚ');
defVowel('ㅜ', 'ㅓ', 'ㅝ');
defVowel('ㅜ', 'ㅔ', 'ㅞ');
defVowel('ㅜ', 'ㅣ', 'ㅟ');
defVowel('ㅡ', 'ㅣ', 'ㅢ');

function composeSyllable(cho, jung, jong) {
  return String.fromCharCode(0xac00 + (cho * 21 + jung) * 28 + jong);
}

class Composer {
  constructor() {
    this.reset();
  }

  reset() {
    this.cho = -1;
    this.jung = -1;
    this.jong = -1;
  }

  /** The single live character currently represented by the state. */
  _render() {
    if (this.cho >= 0 && this.jung >= 0) {
      return composeSyllable(this.cho, this.jung, this.jong < 0 ? 0 : this.jong);
    }
    if (this.cho >= 0) return CHO[this.cho];
    if (this.jung >= 0) return JUNG[this.jung];
    return '';
  }

  input(jamo) {
    if (JUNG_MAP[jamo] !== undefined) return this._inputVowel(JUNG_MAP[jamo]);
    if (CHO_MAP[jamo] !== undefined) return this._inputConsonant(jamo, CHO_MAP[jamo]);
    // Unknown character: finalize whatever we have and emit it literally.
    const commit = this.flush() + jamo;
    return { commit, composing: '' };
  }

  _inputConsonant(jamo, choIdx) {
    const jongIdx = JONG_MAP[jamo]; // undefined for ㄸ/ㅃ/ㅉ

    // Empty → start a fresh initial.
    if (this.cho < 0 && this.jung < 0) {
      this.cho = choIdx;
      return { commit: '', composing: this._render() };
    }
    // Lone consonant already open → commit it, start a new initial.
    if (this.cho >= 0 && this.jung < 0) {
      const commit = CHO[this.cho];
      this.reset();
      this.cho = choIdx;
      return { commit, composing: this._render() };
    }
    // Lone vowel open → commit it, start a new initial.
    if (this.jung >= 0 && this.cho < 0) {
      const commit = JUNG[this.jung];
      this.reset();
      this.cho = choIdx;
      return { commit, composing: this._render() };
    }
    // cho + jung open.
    if (this.jong < 0) {
      if (jongIdx !== undefined) {
        this.jong = jongIdx;
        return { commit: '', composing: this._render() };
      }
      // Cannot be a final → commit syllable, start new initial.
      const commit = this._render();
      this.reset();
      this.cho = choIdx;
      return { commit, composing: this._render() };
    }
    // cho + jung + jong open → try to extend into a compound final.
    const comp = FINAL_COMPOUND[`${this.jong}+${jamo}`];
    if (comp !== undefined) {
      this.jong = comp;
      return { commit: '', composing: this._render() };
    }
    const commit = this._render();
    this.reset();
    this.cho = choIdx;
    return { commit, composing: this._render() };
  }

  _inputVowel(vIdx) {
    // Empty or lone consonant → attach as medial.
    if (this.jung < 0 && this.cho >= 0) {
      this.jung = vIdx;
      return { commit: '', composing: this._render() };
    }
    if (this.cho < 0 && this.jung < 0) {
      this.jung = vIdx;
      return { commit: '', composing: this._render() };
    }
    // Lone vowel open → try compound vowel, else commit and open a new one.
    if (this.jung >= 0 && this.cho < 0) {
      const comp = VOWEL_COMPOUND[`${this.jung}+${JUNG[vIdx]}`];
      if (comp !== undefined) {
        this.jung = comp;
        return { commit: '', composing: this._render() };
      }
      const commit = JUNG[this.jung];
      this.reset();
      this.jung = vIdx;
      return { commit, composing: this._render() };
    }
    // cho + jung, no final → compound vowel, else start a new (lone) vowel.
    if (this.jong < 0) {
      const comp = VOWEL_COMPOUND[`${this.jung}+${JUNG[vIdx]}`];
      if (comp !== undefined) {
        this.jung = comp;
        return { commit: '', composing: this._render() };
      }
      const commit = this._render();
      this.reset();
      this.jung = vIdx;
      return { commit, composing: this._render() };
    }
    // cho + jung + jong → the final migrates to the next syllable's initial.
    if (FINAL_DECOMPOSE[this.jong]) {
      // Compound final: base stays, second part migrates. (닭 + ㅏ → 달가)
      const [base, secondChar] = FINAL_DECOMPOSE[this.jong];
      this.jong = base;
      const commit = this._render();
      this.reset();
      this.cho = CHO_MAP[secondChar];
      this.jung = vIdx;
      return { commit, composing: this._render() };
    }
    // Single final: it migrates whole.
    const migratedCho = JONG_TO_CHO[this.jong];
    this.jong = -1;
    const commit = this._render();
    this.reset();
    this.cho = migratedCho;
    this.jung = vIdx;
    return { commit, composing: this._render() };
  }

  backspace() {
    if (this.cho < 0 && this.jung < 0) return { composing: '', handled: false };
    if (this.jong >= 0) {
      this.jong = FINAL_DECOMPOSE[this.jong] ? FINAL_DECOMPOSE[this.jong][0] : -1;
    } else if (this.jung >= 0) {
      this.jung = VOWEL_DECOMPOSE[this.jung] !== undefined ? VOWEL_DECOMPOSE[this.jung] : -1;
    } else if (this.cho >= 0) {
      this.cho = -1;
    }
    return { composing: this._render(), handled: true };
  }

  flush() {
    const s = this._render();
    this.reset();
    return s;
  }
}

export function createComposer() {
  return new Composer();
}
