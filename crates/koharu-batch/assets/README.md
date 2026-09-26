# Translation test bench for adult manga

Two small, hand-written test sets for measuring how well a translator (DeepL or
a local LLM) and a corrector handle the lines that machine translation gets
wrong in adult manga: sexual double meanings, honorifics, register and who is
speaking to whom.

**These sets target Spanish**: the expected answers are Spanish words, because
the goal was to improve translation into Spanish. The format is not tied to
Spanish, though. The point of sharing it is for each person to build the same
kind of set **for their own target language** and improve translation into it.

All lines are written for the test; none are quoted from a published work. All
characters are adults (the work note in `escenas.json` gives their ages), and
new cases must keep it that way.

## Files

| File | Used by | What it measures |
|---|---|---|
| `escenas.json` | `khr escenas` | 74 lines (en 22, ja 17, ko 17, zh 18) that only the scene before them resolves, e.g. "I'm coming" as arriving vs. climax. Comes with a work note (`ficha`) and a glossary, so it also measures what those add (`--without-notes`). |
| `bench-cases.json` | `khr bench` | 40 isolated lines (10 per source language), each with a typical wrong Spanish version, for scoring a **corrector** on existing Spanish (or a translator with `--translate`). Spanish only. |

## Case format (`escenas.json`)

```json
{
  "idioma": "ja",
  "escena": ["previous balloon", "previous balloon"],
  "ref": "the line to translate (the last balloon of the scene)",
  "check": "contiene",
  "value": ["accepted", "variants"],
  "trampa": "what makes this line hard",
  "control": false
}
```

- `idioma`: source language (`ja`, `ko`, `zh`); omitted means English.
- `escena`: the balloons before the line, in reading order. They go through the translator together with the line, as a real page would.
- `check`: how the translation of `ref` is scored.
  - `contiene`: it must contain at least one of `value` (case-insensitive substrings, so a stem like `veng` covers *vengo* and *venirme*).
  - `no_contiene`: it must contain none of them (and not be empty).

  `bench-cases.json` also has `igual`: the line must stay unchanged.
- `trampa`: a short note on the trap, shown when a case fails.
- `control: true` marks a **control**: the same ambiguous vocabulary in a scene that is *not* sexual. Controls measure over-interpretation. A corrector primed to read everything sexually turns a goodbye into an orgasm, and that error is worse than the one it fixes because nobody reviews those pages. The current set has 27 controls for 47 traps; keep a similar share.

The top-level `ficha` (work note) and `glosario` (tab-separated `source<TAB>target<TAB>note`) are sent with every case unless `--without-notes` is given.

## Building a set for your target language

1. Copy `escenas.json` to e.g. `escenas.fr.json`.
2. Keep `escena` and `ref` in the source languages; they don't depend on the target.
3. Rewrite `value` with the accepted words in **your** target language. Use stems where the language inflects, and list the colloquial options a native reader would accept, not only the textbook one.
4. Translate the `ficha` and the glossary targets into your language, and adapt register notes (honorifics, formal/informal address) to it.
5. Add traps specific to your language, e.g. formal vs. informal "you" and gender agreement.
6. Set Koharu's target language to yours and run:

   ```
   khr escenas --cases escenas.fr.json --provider lm-studio --model <model> --runs 3
   khr escenas --cases escenas.fr.json --provider deepl
   khr escenas --cases escenas.fr.json --model <model> --without-notes
   ```

   `--runs 3` repeats the set to show how stable a model is, and `--resultados results.tsv --etiqueta <name>` appends one row per source language for comparing runs.

## Results so far (Spanish, 16 GB VRAM / 16 GB RAM)

| Setup | Score |
|---|---|
| Gemma 4 12B it QAT, with work note (74 scenes) | en 7/8 · ja 4/5 · zh 4.7/5 · ko 88% |
| DeepL (receives neither note nor glossary) | en 3/8 · ja 2/5 · zh 3/5 · ko 47% |
| Qwen3 8B, Korean | 71% |
| Cydonia 24B v4.3 translating, with / without work note (35 lines) | 93% / 81% |
| DeepL + Cydonia as corrector (35 lines) | 57% → 75% |

When you publish results for another language, please include the model and quantization, the hardware (VRAM/RAM), the number of runs, and the typical errors. A score without the errors behind it is hard to act on.
