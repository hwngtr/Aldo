# The interactive interface

`aldo tui` is a full-screen front end over the same operations the CLI runs. It
is not a second implementation: it calls `actions::search`,
`actions::download` and `actions::reports`, which are the code the commands
call too. The only thing it adds is a way to see and choose.

```sh
aldo
aldo tui
aldo tui -q "The Beach Boys - Surf's Up"
```

A bare `aldo` opens the interface when a terminal is attached, and prints its
help when one is not, which is what a script or a pipe expects. Without `-q` it
opens on the dashboard, and you press `/` to type a search. With `-q` it runs
that search immediately and opens the results, which is the useful form when a
shell alias or a launcher provides the query.

## Screens

Home, Results and Download are screens. Files, Budgets and Sessions are panels
the size of the body: they replace the view rather than floating over it, so
there is one way to look at a list of things. The search prompt, the download
confirmation and the help overlay are the only things that float.

| Screen | What it is for |
|---|---|
| Setup | The first-run sign-in form. Shown only when no SoulSeek account is saved, and it must be finished before the interface is usable. |
| Home | The wordmark, the search box, and whether Discogs, SoulSeek and the library are configured. |
| Results | Two lists: the SoulSeek folders worth downloading, and the Discogs matches. |
| Files | The contents of one folder, with sizes, before committing to a download. |
| Download | The overall bar, the active file's bar and speed, and the state of every file. |
| Budgets | Each provider's remaining allowance as a bar, so a SoulSeek ban is visible before it happens. |
| Sessions | Recent searches, newest first. |
| Help | The key map, on `?`. |

Each panel is as tall as its content, so a search that reached no peers shows a
short explanation rather than one tall empty box.

## Signing in

The interface asks for a SoulSeek account the first time it runs, and saves the
answer to `~/.config/aldo/.env`. There is no menu item for it: once signed in,
credentials are background detail. Signing out is `aldo logout` in a terminal,
after which the next run asks again. The Discogs token on the form is optional
and may be left blank; add one later with
`aldo login --token TOKEN`. Text pasted into any field is accepted, so
a token does not have to be typed by hand.

## Keys

| Key | Does |
|---|---|
| `Tab` / `↑` `↓` (Setup) | Move between the credential fields. `Enter` saves; the password is typed without echo. |
| `/` | Open the search prompt. `Ctrl-A` selects the whole query and `Ctrl-E` jumps to its end; `Backspace` or `Delete` clears a selection. |
| `Enter` | Run the typed search, or open the highlighted item. |
| `Esc`, `h` | Back. Closes the help overlay or the confirmation; otherwise leaves the current panel or screen, and then returns Home. |
| `Tab` | Switch between the Sources and Discogs lists. |
| `j` / `k`, `↓` / `↑` | Move the selection. |
| `g` / `G` | First / last row. |
| `l` | List the files in the highlighted folder. |
| `d` | Download the highlighted folder, after a confirmation. |
| `t`, `Enter` on Discogs | Use the highlighted pressing, then confirm with `d`. |
| `b` / `s` | Rate budgets / recent searches. |
| `r` | Reload the budgets or the sessions list. |
| `?` | Toggle help. |
| `Ctrl-C` | Quit, from anywhere. `q` is deliberately unbound. |

## Two decisions worth knowing

**The search prompt is modal.** While it is open, every character is content.
Single-key commands and typed queries want the same keys, and there is no way to
have both: without a modal prompt, a search for `Bad Brains` would open the
budgets screen on the `b`, `h` would throw you back out, and `?` would open help
mid-word.

**The confirmation is modal for the same reason,** and because a download moves
files. It shows the peer, the file count, the size, the destination, and whether
tags will be written. `Enter` starts it, `Esc` cancels.

## Downloads

`d` resolves the plan and the Discogs tags *before* asking, so the confirmation
can state the destination and whether tags will be written.

A release whose track count does not match the folder is a warning here and a
refusal in `aldo download`. That difference is deliberate: the CLI must not
silently tag a partial folder, while the TUI can show the mismatch and let you
choose to continue untagged. The plan is resolved once, so the confirmation and
the transfer cannot disagree.

A download keeps running if you leave the screen, and the header keeps counting.
A second download is refused while one is running, because two concurrent
clients would be two logins and two shares of the same rate budget.

## Colour

The accents are Anthropic's brand colours, tuned for a dark terminal, which is
where a terminal UI is usually read. They stay legible on a light one, but
secondary text is dimmer there.

Nothing sets a background except the selection highlight: the terminal already
chose its own, and overriding it is how a TUI becomes unreadable. The highlight
fills with the accent and uses near-black text, so it is legible whatever the
scheme.

The muted grey is the one colour that had to be a compromise. It measures 3.4:1
against a light background (the paler grey it replaced was 2.1:1, below even the
3:1 floor for incidental text) and 5.2:1 against a dark one. No single grey
reaches 4.5:1 against both, so it sits at the balance point. A palette that
adapted would need the terminal's own background colour, which this does not
query.

## Fonts

The interface draws characters; the terminal picks the font, and no TUI can
choose its own. Two consequences are worth knowing.

**The wordmark is drawn, not typeset.** It is a blocky mark in the style of
OpenCode's: solid geometric letterforms on a coarse grid, in a single tone. It
lives in `crates/aldo/src/tui/wordmark.rs` as rows of `█` and spaces.

No font is involved, which is the point. A display face would have to be
rasterised and baked in, and then its licence would travel with the derived art.
Drawn directly, the mark appears identically in any terminal, needs no fallback,
and carries no third-party licence. A terminal narrower than 27 columns, or
shorter than twelve rows, gets the plain `A L D O` instead of a clipped logo.

The design is five rows tall, but a terminal cell is about twice as tall as it is
wide, so five rows of full blocks look stretched. The renderer pairs the design
rows into half blocks — `▀`, `▄`, `█` — so every row of the design survives and
the mark occupies three.

**Four symbols need a fallback.** `▸`, `✓`, `✗` and the braille spinner are not
in every monospace face; Paper Mono, for one, has none of them. Let the terminal
fall back, or pin them so the result does not depend on which fonts happen to be
installed:

```conf
# kitty
symbol_map U+25B8,U+2713,U+2717,U+2800-U+28FF JetBrainsMono Nerd Font Mono
```

Everything else the interface draws — box drawing, blocks, `·`, `…`, `—`, `→` —
is present in any reasonable monospace face. That includes `→`, which marks a
running download: the prettier `⏵` (U+23F5) is absent from every font in this
environment, so it was replaced rather than left to render as a blank box.

## Not a terminal

If stdout is not a terminal, `aldo tui` prints one line explaining that and
exits non-zero rather than taking over a pipe. `aldo search` is the right command
for a script.
