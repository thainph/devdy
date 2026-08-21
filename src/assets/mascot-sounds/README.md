# DY Cyber Fox — sound clips

Drop short audio clips here to give the mascot a "voice". A clip plays each time
a speech bubble appears, chosen by the bubble's variant.

## Expected file names (stem only — extension is free)

Supported extensions: `.mp3`, `.wav`, `.ogg`, `.m4a`, `.webm`, `.aac`.

| Stem           | Plays when the bubble is…                          |
| -------------- | -------------------------------------------------- |
| `info`         | a neutral message / manual info bubble             |
| `success`      | a run finished OK (happy chirp)                    |
| `error`        | a run failed / cancelled (worried grumble)         |
| `thinking`     | the fox is busy (thinking / streaming a tool)      |
| `permission`   | waiting for the user's approval (alert chirp)      |
| `default`      | fallback used for any variant without its own clip |

## Multiple takes (random variety)

You can drop several takes per state and one is picked at random each time.
A file belongs to a state when its name is the stem itself OR starts with
`<stem>-` / `<stem>_`:

```
success.mp3      success-1.mp3   success_2.wav    → success
thinking-1.mp3   thinking-2.mp3  thinking-3.mp3   → thinking
default.mp3                                       → fallback
```

## Notes

- Keep clips **short** (≈0.2–0.8s) so they feel like a chirp, not a jingle.
- Missing a variant file is fine — it falls back to `default.*`.
- No files here at all → the mascot simply stays silent.
- Toggle the whole feature in **Settings → Mascot → Sound**.
- Playback volume is set in `src/composables/useMascotSound.ts` (`el.volume`).
